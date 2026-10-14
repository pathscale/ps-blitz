//! Browser-issued resource capabilities. Never derive these from page headers.

use crate::net::Url;
use crate::platform::OriginKey;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Credentials {
    Omit,
    #[default]
    SameOrigin,
    Include,
}

/// Application parameters, captured when a document is created.
#[derive(Clone, Debug)]
pub struct ResourcePolicy {
    pub max_requests: u64,
    pub warn_requests: u64,
    pub max_bytes: u64,
    pub warn_bytes: u64,
    /// Explicit browser permission, never a page's credentials option.
    pub cross_origin_credentials: bool,
}

impl Default for ResourcePolicy {
    fn default() -> Self {
        Self {
            max_requests: 4096,
            warn_requests: 3072,
            max_bytes: 256 * 1024 * 1024,
            warn_bytes: 192 * 1024 * 1024,
            cross_origin_credentials: false,
        }
    }
}

#[derive(Debug, Default)]
struct Usage {
    requests: u64,
    bytes: u64,
}

/// One instance of one document. Opaque file origins are never recomputed.
#[derive(Debug)]
pub struct ResourceScope {
    url: Url,
    origin: OriginKey,
    policy: Arc<ResourcePolicy>,
    usage: Arc<Mutex<Usage>>,
    file_access: bool,
    private_network: bool,
}

/// Only native browser code can issue these; they are not deserializable.
#[derive(Clone, Debug, Default)]
pub enum Initiator {
    #[default]
    Unknown,
    Browser(Arc<ResourceScope>),
    Document(Arc<ResourceScope>),
}

impl ResourcePolicy {
    /// Call only for a user navigation or a browser-owned load.
    pub fn browser(self: &Arc<Self>, url: Url) -> Initiator {
        Initiator::Browser(Arc::new(ResourceScope {
            origin: OriginKey::for_document(&url),
            file_access: url.scheme() == "file",
            private_network: true,
            url,
            policy: Arc::clone(self),
            usage: Arc::new(Mutex::new(Usage::default())),
        }))
    }

    /// Commit the final URL of a navigation. The navigation and its page share
    /// a budget. A web navigation cannot mint a local-file capability.
    pub fn document(self: &Arc<Self>, url: Url, navigation: &Initiator) -> Initiator {
        let scope = navigation.scope();
        let file_access = matches!(navigation, Initiator::Browser(_))
            && scope.is_some_and(|scope| scope.file_access && scope.url == url);
        let private_network = local_host(&url)
            && scope.is_some_and(|scope| scope.private_network);
        Initiator::Document(Arc::new(ResourceScope {
            origin: OriginKey::for_document(&url),
            url,
            policy: Arc::clone(self),
            usage: scope.map_or_else(
                || Arc::new(Mutex::new(Usage::default())),
                |scope| Arc::clone(&scope.usage),
            ),
            file_access,
            private_network,
        }))
    }
}

impl Initiator {
    pub fn scope(&self) -> Option<&Arc<ResourceScope>> {
        match self {
            Self::Unknown => None,
            Self::Browser(scope) | Self::Document(scope) => Some(scope),
        }
    }

    pub fn document_url(&self) -> Option<&Url> {
        self.scope().map(|scope| &scope.url)
    }

    pub fn origin(&self) -> Option<&OriginKey> {
        self.scope().map(|scope| &scope.origin)
    }

    /// A child document receives a new identity, but cannot escape its parent's
    /// budget or gain private-network permission by naming a private URL.
    pub fn child(&self, url: Url) -> Self {
        match self.scope() {
            Some(scope) => scope.policy.document(url, self),
            None => Self::Unknown,
        }
    }

    pub fn check_url(&self, url: &Url) -> Result<(), String> {
        let scope = self.scope().ok_or("resource-initiator-unknown")?;
        if url.scheme() == "file" && !scope.file_access {
            return Err("resource-file-origin".into());
        }
        if matches!(url.scheme(), "http" | "https")
            && local_host(url) && !scope.private_network
        {
            return Err("resource-private-address".into());
        }
        Ok(())
    }

    /// Apply to every resolved candidate before connecting to that exact list.
    pub fn check_address(&self, address: IpAddr) -> Result<(), String> {
        let scope = self.scope().ok_or("resource-initiator-unknown")?;
        if private_address(address) && !scope.private_network {
            return Err("resource-private-address".into());
        }
        Ok(())
    }

    pub fn restrict_network(&self) -> bool {
        self.scope().is_none_or(|scope| !scope.private_network)
    }

    pub fn allows_credentials(&self, url: &Url, mode: Credentials) -> bool {
        let Some(scope) = self.scope() else { return false; };
        mode != Credentials::Omit && (scope.origin.is_persistable()
            && scope.origin == OriginKey::for_document(url)
            || mode == Credentials::Include && scope.policy.cross_origin_credentials)
    }

    pub fn begin(&self, url: &Url) -> Result<(), String> {
        self.check_url(url)?;
        let scope = self.scope().ok_or("resource-initiator-unknown")?;
        let mut usage = scope.usage.lock().unwrap_or_else(|error| error.into_inner());
        if usage.requests >= scope.policy.max_requests {
            return Err("resource-request-cap".into());
        }
        usage.requests += 1;
        if usage.requests == scope.policy.warn_requests {
            eprintln!("resource-request-warning: {} ({})", usage.requests, scope.url);
        }
        Ok(())
    }

    /// Monotonic accounting includes redirects, failures and bytes from cache.
    /// Workers and child documents share this counter with their owner.
    pub fn charge(&self, bytes: usize) -> Result<(), String> {
        let scope = self.scope().ok_or("resource-initiator-unknown")?;
        let mut usage = scope.usage.lock().unwrap_or_else(|error| error.into_inner());
        let bytes = bytes as u64;
        if bytes > scope.policy.max_bytes.saturating_sub(usage.bytes) {
            return Err("resource-byte-cap".into());
        }
        let previous = usage.bytes;
        usage.bytes += bytes;
        if previous < scope.policy.warn_bytes && usage.bytes >= scope.policy.warn_bytes {
            eprintln!("resource-byte-warning: {} ({})", usage.bytes, scope.url);
        }
        Ok(())
    }

    pub fn remaining_bytes(&self) -> usize {
        self.scope().map_or(0, |scope| {
            let usage = scope.usage.lock().unwrap_or_else(|error| error.into_inner());
            usize::try_from(scope.policy.max_bytes.saturating_sub(usage.bytes))
                .unwrap_or(usize::MAX)
        })
    }
}

/// Conservative local origin recognition. DNS names other than localhost and
/// .local remain public principals even when the user navigates to them.
fn local_host(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Ipv4(ip)) => private_address(ip.into()),
        Some(url::Host::Ipv6(ip)) => private_address(ip.into()),
        Some(url::Host::Domain(host)) => {
            let host = host.trim_end_matches('.').to_ascii_lowercase();
            host == "localhost" || host.ends_with(".localhost")
                || host == "local" || host.ends_with(".local")
        }
        None => false,
    }
}

pub fn private_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(ip) => {
            let [a, b, _, _] = ip.octets();
            a == 0 || a == 127 || a == 10 || a == 172 && (16..=31).contains(&b)
                || a == 192 && b == 168 || a == 169 && b == 254
                || a == 100 && (64..=127).contains(&b) || a >= 224
        }
        IpAddr::V6(ip) => {
            if let Some(v4) = ip.to_ipv4_mapped() { return private_address(v4.into()); }
            let octets = ip.octets();
            ip.is_unspecified() || ip.is_loopback() || ip.is_multicast()
                || octets[0] & 0xfe == 0xfc
                || octets[0] == 0xfe && octets[1] & 0xc0 == 0x80
                // Embedded IPv4 transition addresses cannot bypass the v4 rule.
                || ip.segments()[0] == 0x2002
                || ip.segments()[0] == 0x2001 && ip.segments()[1] == 0
                || ip.segments()[0] == 0x64 && ip.segments()[1] == 0xff9b
        }
    }
}

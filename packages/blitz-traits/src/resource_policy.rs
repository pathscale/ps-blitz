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
    file_root: Option<std::path::PathBuf>,
    private_network: bool,
    public_https_domains: Option<Arc<[String]>>,
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
        let file_root = if url.scheme() == "file" {
            canonical_file_root(&url)
        } else {
            None
        };
        Initiator::Browser(Arc::new(ResourceScope {
            origin: OriginKey::for_document(&url),
            file_access: file_root.is_some(),
            file_root,
            private_network: true,
            public_https_domains: None,
            url,
            policy: Arc::clone(self),
            usage: Arc::new(Mutex::new(Usage::default())),
        }))
    }

    /// Commit the final URL of a navigation. The navigation and its page share
    /// a budget. A web navigation cannot mint a local-file capability.
    pub fn document(self: &Arc<Self>, url: Url, navigation: &Initiator) -> Initiator {
        let domains = navigation
            .scope()
            .and_then(|scope| scope.public_https_domains.clone());
        self.document_with_egress(url, navigation, domains)
    }

    /// Create a document principal for untrusted browsing. Each HTTPS hop and
    /// resolved address is checked. An empty allowlist permits any public host.
    /// The restriction follows the document into its children.
    pub fn public_https_document(
        self: &Arc<Self>,
        url: Url,
        navigation: &Initiator,
        allowed_domains: &[String],
    ) -> Initiator {
        self.document_with_egress(url, navigation, Some(Arc::from(allowed_domains)))
    }

    fn document_with_egress(
        self: &Arc<Self>,
        url: Url,
        navigation: &Initiator,
        public_https_domains: Option<Arc<[String]>>,
    ) -> Initiator {
        let scope = navigation.scope();
        let unrestricted = public_https_domains.is_none();
        let file_root = if unrestricted && matches!(navigation, Initiator::Browser(_)) {
            scope
                .filter(|scope| scope.file_access && scope.url == url)
                .and_then(|scope| scope.file_root.clone())
        } else {
            None
        };
        let file_access = file_root.is_some();
        let private_network = unrestricted
            && local_host(&url)
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
            file_root,
            private_network,
            public_https_domains,
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
        if let Some(domains) = &scope.public_https_domains {
            return match url.scheme() {
                // These resources are handled locally and never reach a dialer.
                "data" | "blob" => Ok(()),
                _ => check_public_https_url(url, domains),
            };
        }
        if url.scheme() == "file" {
            if !scope.file_access {
                return Err("resource-file-origin".into());
            }
            let root = scope.file_root.as_ref().ok_or("resource-file-scope")?;
            let path = local_file_path(url).ok_or("resource-file-scope")?;
            let path = std::fs::canonicalize(path).map_err(|_| "resource-file-scope")?;
            if !path.starts_with(root) {
                return Err("resource-file-scope".into());
            }
            let metadata = std::fs::metadata(path).map_err(|_| "resource-file-scope")?;
            if !metadata.is_file() {
                return Err("resource-file-scope".into());
            }
            return Ok(());
        }
        if matches!(self, Self::Document(document) if document.url.scheme() == "file")
            && matches!(url.scheme(), "http" | "https")
        {
            return Err("resource-file-network".into());
        }
        if matches!(url.scheme(), "http" | "https") && local_host(url) && !scope.private_network {
            return Err("resource-private-address".into());
        }
        Ok(())
    }

    /// Apply to every resolved candidate before connecting to that exact list.
    pub fn check_address(&self, address: IpAddr) -> Result<(), String> {
        let scope = self.scope().ok_or("resource-initiator-unknown")?;
        let denied = if scope.public_https_domains.is_some() {
            !public_https_address(address)
        } else {
            private_address(address) && !scope.private_network
        };
        if denied {
            return Err("resource-private-address".into());
        }
        Ok(())
    }

    pub fn public_https_only(&self) -> bool {
        self.scope()
            .is_some_and(|scope| scope.public_https_domains.is_some())
    }

    pub(crate) fn check_redirect_destination(&self, url: &Url) -> Result<(), String> {
        if let Some(domains) = self
            .scope()
            .and_then(|scope| scope.public_https_domains.as_deref())
        {
            check_public_https_url(url, domains)?;
        }
        Ok(())
    }

    pub fn restrict_network(&self) -> bool {
        self.scope().is_none_or(|scope| !scope.private_network)
    }

    pub fn allows_credentials(&self, url: &Url, mode: Credentials) -> bool {
        let Some(scope) = self.scope() else {
            return false;
        };
        mode != Credentials::Omit
            && (scope.origin.is_persistable() && scope.origin == OriginKey::for_document(url)
                || mode == Credentials::Include && scope.policy.cross_origin_credentials)
    }

    pub fn begin(&self, url: &Url) -> Result<(), String> {
        let scope = self.scope().ok_or("resource-initiator-unknown")?;
        let mut usage = scope
            .usage
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if usage.requests >= scope.policy.max_requests {
            return Err("resource-request-cap".into());
        }
        usage.requests += 1;
        if usage.requests == scope.policy.warn_requests {
            eprintln!(
                "resource-request-warning: {} ({})",
                usage.requests, scope.url
            );
        }
        drop(usage);
        self.check_url(url)
    }

    /// Monotonic accounting includes redirects, failures and bytes from cache.
    /// Workers and child documents share this counter with their owner.
    pub fn charge(&self, bytes: usize) -> Result<(), String> {
        let scope = self.scope().ok_or("resource-initiator-unknown")?;
        let mut usage = scope
            .usage
            .lock()
            .unwrap_or_else(|error| error.into_inner());
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
            let usage = scope
                .usage
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            usize::try_from(scope.policy.max_bytes.saturating_sub(usage.bytes))
                .unwrap_or(usize::MAX)
        })
    }
}

fn check_public_https_url(url: &Url, allowed_domains: &[String]) -> Result<(), String> {
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port_or_known_default() == Some(0)
    {
        return Err("resource-public-https-required".into());
    }
    match url.host() {
        Some(url::Host::Domain(host)) => {
            let host = host.trim_end_matches('.');
            if !host.contains('.')
                || [".localhost", ".local", ".internal", ".lan", ".flycast", ".home.arpa"]
                    .into_iter()
                    .any(|suffix| host.ends_with(suffix))
            {
                return Err("resource-private-address".into());
            }
            if allowed_domains.is_empty()
                || allowed_domains.iter().any(|domain| {
                    host == domain.as_str()
                        || host
                            .strip_suffix(domain.as_str())
                            .is_some_and(|prefix| prefix.ends_with('.'))
                })
            {
                Ok(())
            } else {
                Err("resource-host-not-allowed".into())
            }
        }
        Some(url::Host::Ipv4(ip)) if allowed_domains.is_empty() && public_https_address(ip.into()) => {
            Ok(())
        }
        Some(url::Host::Ipv6(ip)) if allowed_domains.is_empty() && public_https_address(ip.into()) => {
            Ok(())
        }
        _ => Err("resource-private-address".into()),
    }
}

fn public_https_address(address: IpAddr) -> bool {
    if private_address(address) {
        return false;
    }
    match address {
        IpAddr::V4(ip) => !matches!(
            ip.octets(),
            [192, 0, 2, _] | [192, 88, 99, _] | [198, 51, 100, _] | [203, 0, 113, _]
        ),
        IpAddr::V6(ip) => {
            if let Some(ip) = ip.to_ipv4_mapped() {
                return public_https_address(ip.into());
            }
            let [first, second, third, ..] = ip.octets();
            let segments = ip.segments();
            first & 0xe0 == 0x20
                && !(segments[0] == 0x2001 && (segments[1] < 0x0200 || segments[1] == 0x0db8))
                && !(first == 0x3f && second == 0xff && third & 0xf0 == 0)
        }
    }
}

fn local_file_path(url: &Url) -> Option<std::path::PathBuf> {
    if url.scheme() != "file"
        || url
            .host_str()
            .is_some_and(|host| !host.eq_ignore_ascii_case("localhost"))
    {
        return None;
    }
    url.to_file_path().ok()
}

fn canonical_file_root(url: &Url) -> Option<std::path::PathBuf> {
    let path = std::fs::canonicalize(local_file_path(url)?).ok()?;
    let metadata = std::fs::metadata(&path).ok()?;
    if !metadata.is_file() {
        return None;
    }
    path.parent().map(|parent| parent.to_path_buf())
}

/// Conservative local origin recognition. DNS names other than localhost and
/// .local remain public principals even when the user navigates to them.
fn local_host(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Ipv4(ip)) => private_address(ip.into()),
        Some(url::Host::Ipv6(ip)) => private_address(ip.into()),
        Some(url::Host::Domain(host)) => {
            let host = host.trim_end_matches('.').to_ascii_lowercase();
            host == "localhost"
                || host.ends_with(".localhost")
                || host == "local"
                || host.ends_with(".local")
        }
        None => false,
    }
}

pub fn private_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            a == 0
                || a == 127
                || a == 10
                || a == 172 && (16..=31).contains(&b)
                || a == 192 && b == 168
                || a == 169 && b == 254
                || a == 100 && (64..=127).contains(&b)
                || a == 198 && (18..=19).contains(&b)
                || a == 192 && b == 0 && c == 0
                || a >= 224
        }
        IpAddr::V6(ip) => {
            if let Some(v4) = ip.to_ipv4_mapped() {
                return private_address(v4.into());
            }
            let octets = ip.octets();
            ip.is_unspecified() || ip.is_loopback() || ip.is_multicast()
                || octets[..12].iter().all(|byte| *byte == 0)
                || octets[0] == 0xfe && octets[1] & 0xc0 == 0xc0
                || octets[0] & 0xfe == 0xfc
                || octets[0] == 0xfe && octets[1] & 0xc0 == 0x80
                // Embedded IPv4 transition addresses cannot bypass the v4 rule.
                || ip.segments()[0] == 0x2002
                || ip.segments()[0] == 0x2001 && ip.segments()[1] == 0
                || ip.segments()[0] == 0x64 && ip.segments()[1] == 0xff9b
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::FetchRequest;

    #[test]
    fn public_https_policy_survives_document_commit_and_redirects() {
        let resources = Arc::new(ResourcePolicy::default());
        let initial = Url::parse("https://supplier.example/search").unwrap();
        let navigation = resources.public_https_document(
            initial.clone(),
            &resources.browser(initial.clone()),
            &["supplier.example".to_owned()],
        );
        let committed = Url::parse("https://fares.supplier.example/results").unwrap();
        let child = navigation.child(committed.clone());
        let mut request = FetchRequest::get(committed.clone());
        request.initiator = child;

        assert!(request.initiator.check_url(&initial).is_ok());
        assert!(
            request
                .check_redirect(&committed.join("/page/2").unwrap())
                .is_ok()
        );
        for target in [
            "http://fares.supplier.example/",
            "https://supplier.example.evil.example/",
            "https://other.example/",
            "https://169.254.169.254/",
            "https://[::ffff:127.0.0.1]/",
            "https://user@supplier.example/",
            "data:text/html,redirect",
        ] {
            assert!(
                request.check_redirect(&Url::parse(target).unwrap()).is_err(),
                "accepted {target}"
            );
        }
    }
}

//! Native trust anchors and nagoya socket transport.
//!
//! DNS always runs on a nagoya worker, including when the fetch future is
//! polled directly by a caller or a local reactor.

use super::{Error, pool::Origin};
use blitz_traits::tls_policy::TlsPolicyError;
use bytes::BytesMut;
use nago_rustls::{
    TlsSession,
    rustls::{ClientConfig, ClientConnection, RootCertStore},
    rustls_pki_types::ServerName,
};
use nagoya::{net::TcpStream, reactor::Handle};
use ps_rustcrypto::policy;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
};

const READ_CHUNK: usize = 16 * 1024;

pub(super) fn default_config() -> Result<Arc<ClientConfig>, Error> {
    static CONFIG: OnceLock<Result<Arc<ClientConfig>, String>> = OnceLock::new();
    match CONFIG.get_or_init(build_config) {
        Ok(config) => Ok(config.clone()),
        Err(error) => Err(Error::Tls(error.clone())),
    }
}

fn native_roots() -> Result<RootCertStore, String> {
    let native = rustls_native_certs::load_native_certs();
    let mut roots = RootCertStore::empty();
    let (accepted, rejected) = roots.add_parsable_certificates(native.certs);
    if accepted == 0 {
        return Err(format!(
            "no usable native TLS trust anchors: {} loading errors, {rejected} rejected certificates",
            native.errors.len(),
        ));
    }
    if !native.errors.is_empty() || rejected != 0 {
        eprintln!(
            "TLS trust store: {} loading errors, {rejected} rejected certificates",
            native.errors.len(),
        );
    }
    #[cfg(feature = "sandbox2-witness")]
    crate::chacha_policy::add_witness_roots(&mut roots)?;
    Ok(roots)
}

/// The native roots that are also built-in public roots, matched by subject
/// and key. Like Chrome, CT is enforced only on chains ending in one of
/// these; locally added anchors (enterprise or test CAs) skip it.
fn public_roots(native: &RootCertStore) -> RootCertStore {
    let mut public = RootCertStore::empty();
    public.roots = native
        .roots
        .iter()
        .filter(|root| {
            webpki_roots::TLS_SERVER_ROOTS.iter().any(|built_in| {
                root.subject.as_ref() == built_in.subject.as_ref()
                    && root.subject_public_key_info.as_ref()
                        == built_in.subject_public_key_info.as_ref()
            })
        })
        .cloned()
        .collect();
    public
}

/// HTTP/2 preferred; HTTP/1.1 for servers that do not offer it.
pub(super) fn alpn() -> Vec<Vec<u8>> {
    vec![b"h2".to_vec(), b"http/1.1".to_vec()]
}

fn build_config() -> Result<Arc<ClientConfig>, String> {
    let roots = native_roots()?;
    let public = public_roots(&roots);
    let provider = crate::chacha_provider();
    let mut config = ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])
        .map_err(|error| error.to_string())?
        .dangerous()
        .with_custom_certificate_verifier(crate::certificate_verifier(roots, public)?)
        .with_no_client_auth();
    config.alpn_protocols = alpn();
    #[cfg(feature = "sandbox2-witness")]
    if std::env::var("SANDBOX2_TLS_DEFECT").ok().as_deref() == Some("chacha-load") {
        // Plant a real incompatible ALPN in the production TLS entry point.
        config.alpn_protocols = vec![b"sandbox2-incompatible".to_vec()];
    }
    Ok(Arc::new(config))
}

/// Connection counters and the latest TLS policy refusal for each host.
static HOSTS: Mutex<Option<HashMap<String, HostStats>>> = Mutex::new(None);

/// Connection counters for one host, since the process started.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HostStats {
    /// New TCP connections opened, TLS attempts included.
    pub connections: u32,
    /// Handshakes refused under the strict TLS policy.
    pub refusals: u32,
    /// TLS handshakes that completed under the strict policy.
    pub handshakes: u32,
    /// The latest TLS policy refusal, cleared by the next TLS attempt.
    pub last_tls_policy_error: Option<TlsPolicyError>,
    /// The protocol the latest connection to this host settled on: `h2`,
    /// `h2 (coalesced)` onto another host's connection, or `http/1.1`.
    pub last_alpn: Option<&'static str>,
}

fn update(host: &str, change: impl FnOnce(&mut HostStats)) -> HostStats {
    let Ok(mut map) = HOSTS.lock() else {
        return HostStats::default();
    };
    let stats = map
        .get_or_insert_with(HashMap::new)
        .entry(host.to_ascii_lowercase())
        .or_default();
    change(stats);
    *stats
}

pub(super) fn record_alpn(host: &str, alpn: &'static str) {
    update(host, |stats| stats.last_alpn = Some(alpn));
}

/// Connection counters for every host contacted so far, sorted by host.
pub fn connection_stats() -> Vec<(String, HostStats)> {
    let mut all: Vec<_> = HOSTS
        .lock()
        .ok()
        .and_then(|map| {
            map.as_ref()
                .map(|map| map.iter().map(|(h, s)| (h.clone(), *s)).collect())
        })
        .unwrap_or_default();
    all.sort_by(|a, b| a.0.cmp(&b.0));
    all
}

pub(super) async fn open(
    origin: &Origin,
    handle: &Handle,
    config: Arc<ClientConfig>,
    addresses: &[nagoya::reactor::socket::Addr],
) -> Result<Transport, Error> {
    if !origin.tls {
        return Ok(Transport::Plain(connect(origin, handle, addresses).await?));
    }
    update(&origin.host, |stats| stats.last_tls_policy_error = None);
    // One connection and one policy. A refusal is terminal for this request.
    match handshake(origin, handle, config, addresses).await {
        Ok(tls) => {
            update(&origin.host, |stats| stats.handshakes += 1);
            Ok(Transport::Tls(tls))
        }
        Err(error) => {
            if let Error::TlsPolicy(reason) = &error {
                update(&origin.host, |stats| {
                    stats.refusals += 1;
                    stats.last_tls_policy_error = Some(*reason);
                });
            }
            // Resource loads may discard the returned error, so log it here.
            info(format_args!("TLS {}: {error}", origin.host));
            Err(error)
        }
    }
}

fn info(message: std::fmt::Arguments<'_>) {
    #[cfg(feature = "tracing")]
    tracing::info!("{message}");
    #[cfg(not(feature = "tracing"))]
    eprintln!("info: {message}");
}

pub(super) async fn resolve_addresses(origin: &Origin, initiator: &blitz_traits::net::Initiator) -> Result<Vec<nagoya::reactor::socket::Addr>, Error> {
    let host = origin.host.clone();
    let port = origin.port;
    let addresses = nagoya::spawn(async move {
        nagoya::net::resolve(&host, port).map_err(|error| Error::Dns(error.to_string()))
    })
    .await
    .ok_or_else(|| Error::Dns("resolver task was cancelled".to_owned()))??;

    for address in &addresses {
        use nagoya::reactor::socket::Addr;
        let ip = match address {
            Addr::V4(bytes, _) => std::net::IpAddr::from(*bytes),
            Addr::V6(bytes, _) => std::net::IpAddr::from(*bytes),
            Addr::Path(_) => return Err(Error::Blocked("resource-private-address".into())),
        };
        initiator.check_address(ip).map_err(Error::Blocked)?;
    }
    Ok(addresses)
}

async fn connect(origin: &Origin, handle: &Handle, addresses: &[nagoya::reactor::socket::Addr]) -> Result<TcpStream, Error> {
    let stats = update(&origin.host, |stats| stats.connections += 1);
    info(format_args!(
        "net {}: new connection #{} (tls {})",
        origin.host, stats.connections, origin.tls
    ));
    nagoya::net::connect_any(addresses, handle)
        .await
        .map_err(|error| Error::Io(error.to_string()))
}

/// One TLS connection attempt, retaining the reason for any policy refusal.
async fn handshake(
    origin: &Origin,
    handle: &Handle,
    config: Arc<ClientConfig>,
    addresses: &[nagoya::reactor::socket::Addr],
) -> Result<Box<TlsSession<TcpStream>>, Error> {
    let stream = connect(origin, handle, addresses).await?;
    let name = ServerName::try_from(origin.host.to_string())
        .map_err(|_| Error::InvalidRequest("invalid TLS server name"))?;
    let session =
        ClientConnection::new(config, name).map_err(|error| Error::Tls(error.to_string()))?;
    let mut tls = TlsSession::client(stream, session);
    if let Err(errno) = tls.handshake().await {
        return Err(handshake_failure(tls.tls_error(), errno));
    }
    if tls
        .alpn_protocol()
        .is_some_and(|protocol| protocol != b"http/1.1" && protocol != b"h2")
    {
        return Err(Error::Tls(
            "peer negotiated an unsupported ALPN protocol".to_owned(),
        ));
    }
    #[cfg(feature = "sandbox2-witness")]
    if std::env::var("SANDBOX2_TLS_DEFECT").ok().as_deref() == Some("chacha-load") {
        // Plant the acceptance defect after a real successful handshake.
        return Err(Error::TlsPolicy(TlsPolicyError::CipherSuite));
    }
    Ok(Box::new(tls))
}

/// Preserve the observed reason without probing a second, weaker connection.
///
/// An alert 40 or 71 does not say whether the server needs RSA, AES, or some
/// other algorithm. Report the rejected profile without inventing that fact.
fn handshake_failure(
    error: Option<&nago_rustls::rustls::Error>,
    errno: impl std::fmt::Display,
) -> Error {
    use nago_rustls::rustls::Error as TlsError;
    use nago_rustls::rustls::{
        AlertDescription, CertificateError, PeerIncompatible, PeerMisbehaved,
    };

    let reason = match error {
        Some(error) if policy::is_rsa_refusal(error) => TlsPolicyError::RsaCertificateChain,
        Some(error) if policy::is_key_type_refusal(error) => TlsPolicyError::CertificateKey,
        Some(TlsError::InvalidCertificate(
            CertificateError::UnsupportedSignatureAlgorithmContext { .. }
            | CertificateError::UnsupportedSignatureAlgorithmForPublicKeyContext { .. },
        )) => TlsPolicyError::CertificateSignature,
        Some(TlsError::PeerMisbehaved(PeerMisbehaved::SelectedUnofferedCipherSuite)) => {
            TlsPolicyError::CipherSuite
        }
        Some(TlsError::PeerMisbehaved(
            PeerMisbehaved::SignedHandshakeWithUnadvertisedSigScheme,
        ))
        | Some(TlsError::PeerIncompatible(PeerIncompatible::NoSignatureSchemesInCommon)) => {
            TlsPolicyError::HandshakeSignature
        }
        Some(TlsError::AlertReceived(AlertDescription::HandshakeFailure)) => {
            TlsPolicyError::PeerRejectedAlgorithms { alert: 40 }
        }
        Some(TlsError::AlertReceived(AlertDescription::InsufficientSecurity)) => {
            TlsPolicyError::PeerRejectedAlgorithms { alert: 71 }
        }
        Some(error) if policy::ct::is_not_logged(error) => {
            return Error::Tls("certificate not publicly logged".to_owned());
        }
        Some(error) => return Error::Tls(format!("handshake: {error}")),
        None => return Error::Tls(format!("handshake: {errno}")),
    };
    Error::TlsPolicy(reason)
}

pub(super) enum Transport {
    Plain(TcpStream),
    Tls(Box<TlsSession<TcpStream>>),
}

impl Transport {
    pub async fn read_into(&mut self, buffer: &mut BytesMut) -> Result<usize, Error> {
        match self {
            Self::Plain(stream) => {
                // Read directly into the destination's spare capacity.
                buffer.reserve(READ_CHUNK);
                stream
                    .read_buf(buffer)
                    .await
                    .map_err(|error| Error::Io(error.to_string()))
            }
            Self::Tls(stream) => {
                // TlsSession exposes an initialised plaintext slice rather
                // than a read_buf operation. Avoid a second staging buffer.
                let start = buffer.len();
                buffer.resize(start + READ_CHUNK, 0);
                match stream.read(&mut buffer[start..]).await {
                    Ok(read) => {
                        buffer.truncate(start + read);
                        Ok(read)
                    }
                    Err(error) => {
                        buffer.truncate(start);
                        Err(Error::Io(error.to_string()))
                    }
                }
            }
        }
    }

    pub async fn write_all(&mut self, bytes: &[u8]) -> Result<(), Error> {
        match self {
            Self::Plain(stream) => stream
                .write_all(bytes)
                .await
                .map_err(|error| Error::Write(error.to_string())),
            Self::Tls(stream) => stream
                .write_all(bytes)
                .await
                .map_err(|error| Error::Write(error.to_string())),
        }
    }
}

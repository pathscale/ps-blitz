//! Native trust anchors and nagoya socket transport.
//!
//! DNS always runs on a nagoya worker, including when the fetch future is
//! polled directly by a caller or a local reactor.

use super::{Error, pool::Origin};
use bytes::BytesMut;
use nago_rustls::{
    TlsSession,
    rustls::{ClientConfig, ClientConnection, RootCertStore},
    rustls_pki_types::ServerName,
};
use nagoya::{net::TcpStream, reactor::Handle};
use ps_rustcrypto::{
    fallback,
    policy::{self, ct::CtVerifier},
};
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

/// The RSA fallback policy, for a second connection after an ECC-only refusal.
///
/// Every native root, RSA ones included; certificate signatures may be RSA
/// PKCS#1 v1.5 and the server CertificateVerify RSA-PSS. CT still applies.
pub(super) fn rsa_fallback_config() -> Result<Arc<ClientConfig>, Error> {
    static CONFIG: OnceLock<Result<Arc<ClientConfig>, String>> = OnceLock::new();
    match CONFIG.get_or_init(build_rsa_fallback_config) {
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
    Ok(roots)
}

fn build_rsa_fallback_config() -> Result<Arc<ClientConfig>, String> {
    let roots = native_roots()?;
    let verifier = fallback::rsa_fallback_verifier(roots)
        .map_err(|error| format!("TLS trust store: {error}"))?;
    let provider = Arc::new(ps_rustcrypto::provider_with_rsa_pss());
    let mut config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|error| error.to_string())?
        .dangerous()
        .with_custom_certificate_verifier(CtVerifier::new(verifier))
        .with_no_client_auth();
    config.alpn_protocols = alpn();
    Ok(Arc::new(config))
}

/// HTTP/2 preferred; HTTP/1.1 for servers that do not offer it.
pub(super) fn alpn() -> Vec<Vec<u8>> {
    vec![b"h2".to_vec(), b"http/1.1".to_vec()]
}

fn build_config() -> Result<Arc<ClientConfig>, String> {
    let roots = native_roots()?;
    // Explicit even when an embedder installed another process default.
    let provider = Arc::new(ps_rustcrypto::provider());
    let mut config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|error| error.to_string())?
        // ECC-only: a chain holding an RSA key anywhere, anchor included, is
        // refused with unsupported_certificate before any signature check.
        // A valid chain must then be publicly logged under the CT policy.
        .dangerous()
        .with_custom_certificate_verifier(
            CtVerifier::ecc_only(roots).map_err(|error| format!("TLS trust store: {error}"))?,
        )
        .with_no_client_auth();
    config.alpn_protocols = alpn();
    Ok(Arc::new(config))
}

/// How the TLS connection to each host most recently went: `true` when it
/// needed the RSA fallback, `false` when ECC-only succeeded.
///
/// A record for display and inspection only. It is never consulted before a
/// handshake: every new connection is ECC-only first, so the server sees the
/// refusal (alert 43) each time.
static HOSTS: Mutex<Option<HashMap<String, HostStats>>> = Mutex::new(None);

/// Connection counters for one host, since the process started.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HostStats {
    /// New TCP connections opened, TLS attempts and fallbacks included.
    pub connections: u32,
    /// ECC-only handshakes that ended in the RSA refusal or alert 40.
    pub refusals: u32,
    /// RSA fallback connections that completed.
    pub fallbacks: u32,
    /// Whether the latest completed TLS connection used the RSA fallback.
    pub last_rsa_fallback: Option<bool>,
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

fn record(host: &str, used_fallback: bool) {
    update(host, |stats| {
        stats.last_rsa_fallback = Some(used_fallback);
        stats.fallbacks += u32::from(used_fallback);
    });
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

/// Whether the latest TLS connection to `host` used the RSA fallback.
///
/// `None` when no TLS connection to it has completed.
pub fn rsa_fallback_state(host: &str) -> Option<bool> {
    HOSTS
        .lock()
        .ok()?
        .as_ref()?
        .get(&host.to_ascii_lowercase())
        .and_then(|stats| stats.last_rsa_fallback)
}

pub(super) async fn open(
    origin: &Origin,
    handle: &Handle,
    config: Arc<ClientConfig>,
    rsa_fallback: Option<Arc<ClientConfig>>,
) -> Result<Transport, Error> {
    if !origin.tls {
        return Ok(Transport::Plain(connect(origin, handle).await?));
    }
    // ECC-only first, on every new connection. Never skipped from memory.
    let error = match handshake(origin, handle, config).await? {
        Ok(tls) => {
            record(&origin.host, false);
            return Ok(Transport::Tls(tls));
        }
        Err(error) => error,
    };
    let Some(rsa_fallback) = rsa_fallback.filter(|_| {
        error
            .rustls
            .as_ref()
            .is_some_and(fallback::wants_rsa_fallback)
    }) else {
        return Err(Error::Tls(error.message));
    };
    let host = &origin.host;
    let reason = if error.rustls.as_ref().is_some_and(policy::is_rsa_refusal) {
        "refused the RSA certificate chain, alert 43 unsupported_certificate sent"
    } else if matches!(
        error.rustls,
        Some(nago_rustls::rustls::Error::AlertReceived(_))
    ) {
        "server rejected the ECC-only hello with alert 40 handshake_failure"
    } else {
        "server answered the ECC-only hello with a non-handshake record"
    };
    update(host, |stats| stats.refusals += 1);
    info(format_args!("TLS {host}: ECC-only {reason}"));
    info(format_args!(
        "TLS {host}: RSA fallback, new connection allowing RSA-PSS CertificateVerify and RSA certificate signatures"
    ));
    match handshake(origin, handle, rsa_fallback).await? {
        Ok(tls) => {
            record(host, true);
            Ok(Transport::Tls(tls))
        }
        Err(error) => {
            // Logged here as well as returned: a page that drops the error
            // otherwise leaves the host looking like a stalled connection.
            info(format_args!("TLS {host}: RSA fallback failed: {}", error.message));
            Err(Error::Tls(format!("RSA fallback: {}", error.message)))
        }
    }
}

fn info(message: std::fmt::Arguments<'_>) {
    #[cfg(feature = "tracing")]
    tracing::info!("{message}");
    #[cfg(not(feature = "tracing"))]
    eprintln!("info: {message}");
}

struct Failed {
    message: String,
    rustls: Option<nago_rustls::rustls::Error>,
}

async fn connect(origin: &Origin, handle: &Handle) -> Result<TcpStream, Error> {
    let host = origin.host.clone();
    let port = origin.port;
    let addresses = nagoya::spawn(async move {
        nagoya::net::resolve(&host, port).map_err(|error| Error::Dns(error.to_string()))
    })
    .await
    .ok_or_else(|| Error::Dns("resolver task was cancelled".to_owned()))??;

    let stats = update(&origin.host, |stats| stats.connections += 1);
    info(format_args!(
        "net {}: new connection #{} (tls {})",
        origin.host, stats.connections, origin.tls
    ));
    nagoya::net::connect_any(&addresses, handle)
        .await
        .map_err(|error| Error::Io(error.to_string()))
}

/// One TLS connection attempt. The outer error is a transport failure; the
/// inner one is the handshake failing, with the rustls error kept.
async fn handshake(
    origin: &Origin,
    handle: &Handle,
    config: Arc<ClientConfig>,
) -> Result<Result<Box<TlsSession<TcpStream>>, Failed>, Error> {
    let stream = connect(origin, handle).await?;
    let name = ServerName::try_from(origin.host.to_string())
        .map_err(|_| Error::InvalidRequest("invalid TLS server name"))?;
    let session =
        ClientConnection::new(config, name).map_err(|error| Error::Tls(error.to_string()))?;
    let mut tls = TlsSession::client(stream, session);
    if let Err(errno) = tls.handshake().await {
        return Ok(Err(Failed {
            message: handshake_failure(tls.tls_error(), errno),
            rustls: tls.tls_error().cloned(),
        }));
    }
    if tls
        .alpn_protocol()
        .is_some_and(|protocol| protocol != b"http/1.1" && protocol != b"h2")
    {
        return Err(Error::Tls(
            "peer negotiated an unsupported ALPN protocol".to_owned(),
        ));
    }
    Ok(Ok(Box::new(tls)))
}

/// Why a handshake failed, in words a page can show.
///
/// The errno only says the session is over; the rustls error kept by the
/// session says why.
fn handshake_failure(
    error: Option<&nago_rustls::rustls::Error>,
    errno: impl std::fmt::Display,
) -> String {
    match error {
        Some(error) if policy::is_rsa_refusal(error) => {
            "site requires RSA, blocked by policy (alert 43 unsupported_certificate)".to_owned()
        }
        Some(error) if policy::is_key_type_refusal(error) => {
            "site certificate key is not ECDSA or Ed25519, blocked by policy (alert 43 unsupported_certificate)".to_owned()
        }
        Some(error) if policy::ct::is_not_logged(error) => "certificate not publicly logged".to_owned(),
        Some(error) => format!("handshake: {error}"),
        None => format!("handshake: {errno}"),
    }
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

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
use ps_rustcrypto::policy::{self, ct::CtVerifier};
use std::sync::{Arc, OnceLock};

const READ_CHUNK: usize = 16 * 1024;

pub(super) fn default_config() -> Result<Arc<ClientConfig>, Error> {
    static CONFIG: OnceLock<Result<Arc<ClientConfig>, String>> = OnceLock::new();
    match CONFIG.get_or_init(build_config) {
        Ok(config) => Ok(config.clone()),
        Err(error) => Err(Error::Tls(error.clone())),
    }
}

fn build_config() -> Result<Arc<ClientConfig>, String> {
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
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

pub(super) async fn open(
    origin: &Origin,
    handle: &Handle,
    config: Arc<ClientConfig>,
) -> Result<Transport, Error> {
    let host = origin.host.clone();
    let port = origin.port;
    let addresses = nagoya::spawn(async move {
        nagoya::net::resolve(&host, port).map_err(|error| Error::Dns(error.to_string()))
    })
    .await
    .ok_or_else(|| Error::Dns("resolver task was cancelled".to_owned()))??;

    let stream = nagoya::net::connect_any(&addresses, handle)
        .await
        .map_err(|error| Error::Io(error.to_string()))?;
    if !origin.tls {
        return Ok(Transport::Plain(stream));
    }

    let name = ServerName::try_from(origin.host.to_string())
        .map_err(|_| Error::InvalidRequest("invalid TLS server name"))?;
    let session =
        ClientConnection::new(config, name).map_err(|error| Error::Tls(error.to_string()))?;
    let mut tls = TlsSession::client(stream, session);
    if let Err(errno) = tls.handshake().await {
        return Err(Error::Tls(handshake_failure(tls.tls_error(), errno)));
    }
    if tls
        .alpn_protocol()
        .is_some_and(|protocol| protocol != b"http/1.1")
    {
        return Err(Error::Tls(
            "peer negotiated an unsupported ALPN protocol".to_owned(),
        ));
    }
    Ok(Transport::Tls(Box::new(tls)))
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

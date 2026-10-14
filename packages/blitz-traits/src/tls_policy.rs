//! Typed reasons for refusing TLS under the house algorithm policy.

/// ChaCha20-Poly1305 encryption with ECDSA/Ed25519/RSA signature verification.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TlsPolicyError {
    /// An RSA key appeared in the leaf, intermediates, or required trust anchor.
    RsaCertificateChain,
    /// A certificate key was neither ECDSA nor Ed25519.
    CertificateKey,
    /// A certificate signature was outside the permitted verification algorithms.
    CertificateSignature,
    /// A TLS handshake signature was outside the advertised signature schemes.
    HandshakeSignature,
    /// The peer selected a cipher suite outside the ChaCha20-Poly1305 offer.
    CipherSuite,
    /// The peer rejected the permitted profile with a TLS alert.
    ///
    /// The alert does not distinguish RSA-only from AES-only servers.
    PeerRejectedAlgorithms { alert: u8 },
}

impl std::fmt::Display for TlsPolicyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RsaCertificateChain => f.write_str(
                "RSA certificate chain forbidden; ECDSA/Ed25519 keys required throughout the chain (alert 43 unsupported_certificate)",
            ),
            Self::CertificateKey => f.write_str(
                "certificate key forbidden; only ECDSA/Ed25519 keys allowed (alert 43 unsupported_certificate)",
            ),
            Self::CertificateSignature => f.write_str(
                "certificate signature forbidden; ECDSA/Ed25519/RSA signature verification required",
            ),
            Self::HandshakeSignature => f.write_str(
                "TLS handshake signature forbidden; only advertised ECDSA/Ed25519/RSA verification schemes allowed",
            ),
            Self::CipherSuite => f.write_str(
                "tls-chacha-required: peer selected an unoffered cipher suite; ChaCha20-Poly1305 required",
            ),
            Self::PeerRejectedAlgorithms { alert } => write!(
                f,
                "tls-chacha-required: peer rejected the ChaCha20-Poly1305 offer (TLS alert {alert}); no fallback",
            ),
        }
    }
}

impl std::error::Error for TlsPolicyError {}

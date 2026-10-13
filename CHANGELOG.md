# Changelog

## 0.6.0

- Breaking change: HTTPS requires ECDSA/Ed25519 authentication and ChaCha20-Poly1305 encryption. RSA-only sites now fail with `TlsPolicyError`; RSA fallback and AES support are removed.
- Remove `rsa_fallback_state` and `with_rsa_fallback`. Replace `HostStats.fallbacks` and `HostStats.last_rsa_fallback` with `handshakes` and `last_tls_policy_error`.
- Preserve typed TLS policy refusals in `FetchError::TlsPolicy` and `ProviderError::TlsPolicy`.

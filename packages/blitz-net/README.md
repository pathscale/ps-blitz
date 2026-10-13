# blitz-net TLS policy

HTTPS uses the ps-rustcrypto 0.4 base provider, supplied by the sibling
`ps-rustls-rustcrypto` checkout through a path dependency. The provider is
explicit and does not inherit a process crypto default. Its fallback feature
is disabled.

The TLS offer contains exactly these suites:

- `TLS13_CHACHA20_POLY1305_SHA256`
- `TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256`

Certificate verification and TLS handshake signatures use ECDSA P-256/P-384
and Ed25519 only. The ECC-only verifier checks keys throughout the presented
chain and restricts trust anchors to ECC keys. Native trust anchors remain
the trust source; public-root chains must also satisfy Certificate
Transparency. RSA certificate signatures are not verified.

A connection that fails this policy is terminal. There is no second
connection with weaker algorithms. A peer that offers only RSA or AES cannot
complete TLS. `h2` remains first in ALPN, followed by `http/1.1`. HTTP/1.1
connection limits, timeouts and framing are unchanged.

Policy refusals are available as
`blitz_traits::platform::FetchError::TlsPolicy(TlsPolicyError)` and
`blitz_net::ProviderError::TlsPolicy(TlsPolicyError)`. The error type is
re-exported by `blitz_net`. For resource loads whose callback has no error
channel, the TLS transport logs the refusal. `connection_stats()` exposes
completed handshakes, refusal counts and the latest typed policy refusal.

A server's TLS alert 40 or 71 does not identify which algorithm it requires.
`PeerRejectedAlgorithms { alert }` reports the observed alert and the
ECDSA/Ed25519 plus ChaCha20-Poly1305 profile that was rejected. It names the
RSA/AES prohibition without claiming to distinguish RSA-only from AES-only.
An unoffered cipher selection produces `CipherSuite`; an unadvertised TLS
signature produces `HandshakeSignature`; an RSA key in a chain produces
`RsaCertificateChain`. Other certificate-key and certificate-signature
refusals have their own variants. Ordinary I/O, unrelated TLS failures and
CT failures retain their existing diagnostics.

The former `rsa_fallback_state` export and `HostStats.fallbacks` and
`HostStats.last_rsa_fallback` fields are removed. Callers should use
`HostStats.last_tls_policy_error` and `HostStats.handshakes`.

`deny-crypto.toml` is separate from the Linux headless system-library policy
in `deny.toml`. It includes GUI packages and all targets and rejects both
named RSA/AES crates and providers that bundle those implementations.
Invoke the existing cargo-deny tool with
`cargo deny --all-features check --config deny-crypto.toml bans` when resolving
the workspace graph. This policy is not yet wired into the existing manual
CI workflow. Any CI checkout consuming the path dependency must supply
`ps-rustls-rustcrypto` beside `ps-blitz`.

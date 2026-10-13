# Runtime-free network proposal

This is a historical HTTP/1.1-only proposal against the source base below.
Do not apply it to current master: `packages/blitz-net/src/tls.rs` and
`h2.rs` contain the active strict TLS policy and HTTP/2 transport. The archived
patch's TLS recipe now uses the base ps-rustcrypto provider so it cannot
reintroduce a process-default RSA/AES provider. Keeping the proposal here does
not change the compiled network provider.

Source base: `5fd4633ccc4d53351b50ca5685761b5c436d4001`.
Source head: `de471e45650681509aea8fd871019981c7ce9be3`.
Five commits replace the network package's Tokio-based client with a Nagoya HTTP/1.1 transport and WorkTable-backed cache. This is package-scoped, not workspace-wide Tokio removal.

Before activation:

- Remove plaintext request Cookie values from persisted cache-variant metadata; review authenticated-response cache isolation.
- Bound individual entries and total disk use, with eviction. A per-entry limit alone is insufficient.
- Restore equivalent user-agent, redirect/cookie, concurrent callback, and data/file URL regression coverage removed by the proposal.
- Sanitize consumed framing and hop-by-hop headers on live responses, including
  Connection-nominated headers. Dechunking the body while retaining
  Transfer-Encoding is not a valid response contract; cache-only filtering
  does not fix it. Cover this with a live chunked-response regression.
- Account explicitly for HTTP/2, proxy and zstd regressions. The proposed HTTP/2 feature is not an implementation.
- Verify TLS, HTTP/1.1 framing, redirects, cancellation and cache semantics with the actual consumer, then run both default-disabled and all-feature package checks.

Do not apply this patch merely because it applies cleanly. The SVG change in this PR is independent and can ship without this proposal.

# Changelog

## 0.6.2

- Make AbortSignal reactive: abort wakes every registered request without timers, and dropped waiters unregister.
- Accept an issuer-owned abort signal on FetchRequest and expose Provider::fetch_platform_async, preserving HTTP error responses.
- Reset unfinished HTTP/2 streams with RST_STREAM CANCEL and omit completion callbacks after request abort. Async callers receive ProviderError::Abort or FetchError::Abort.
- Keep admitted cache publications and invalidations alive until persistence completes; aborted requests cannot publish partial bodies or poison later cache lookups.

## 0.6.1

- Honor passive event listeners in blitz-script. Ignore preventDefault() and returnValue = false during passive callbacks, including native and shadow DOM dispatch, while preserving an existing defaultPrevented value and listener matching.
- Default wheel, mousewheel, touchstart and touchmove listeners to passive on window, Document, its document element and its body; explicit passive: false opts out.
- Preserve the window wheel gesture phase in BlitzWheelEvent with the new BlitzWheelPhase field. Forward Started, Moved, Ended and Cancelled from Winit; retain a Momentum value for input sources that can distinguish inertia.

## 0.6.0

- Breaking change: HTTPS requires ECDSA/Ed25519 authentication and ChaCha20-Poly1305 encryption. RSA-only sites now fail with `TlsPolicyError`; RSA fallback and AES support are removed.
- Remove `rsa_fallback_state` and `with_rsa_fallback`. Replace `HostStats.fallbacks` and `HostStats.last_rsa_fallback` with `handshakes` and `last_tls_policy_error`.
- Preserve typed TLS policy refusals in `FetchError::TlsPolicy` and `ProviderError::TlsPolicy`.

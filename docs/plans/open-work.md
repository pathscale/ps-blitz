# Open work

This file tracks requirements that remain open; verify each against the current code before changing behavior.

## DOM child-list clone optimization

Revisit the historical child-list clone optimization only after establishing that recursive cloning cannot mutate the source child list. Measure the affected clone path before claiming a performance improvement. Preserve current behavior until correctness and performance are demonstrated.

## Package and API release review

This PR removes public `ScriptDocument` and `ScriptBackend` and replaces the concrete
`ApplicationDocument` with a generic backend type. The published `ps-blitz-script 0.6.5`
has the old Boa API, so this branch now bumps the workspace to `0.7.0` and its internal
workspace dependency requirements to `^0.7.0`. The manual publish workflow remains an
explicit release action; this PR does not publish packages. Check the selected publish set
and the 0.7.0 dependency closure before any registry release.

## JPEG XL per-document memory budget

The shared `decode_jxl` path refuses a single image whose conservative 64-byte-per-pixel
working-set estimate exceeds 64 MiB before it renders. Its RGBA8 result also has the
existing 64 MiB output cap. These checks bound one decode; they do not impose an aggregate
per-document budget across multiple images, nor do they account for every internal decoder
allocation as a hard process-memory ceiling. Add an owner-managed shared budget before a
document may decode another image, charge and release it on every success and error path,
and verify concurrent requests cannot exceed the cap. Do not describe the current
per-image guard as an aggregate limit.

## Debug-token environment inheritance

`packages/blitz-script/src/debug_control.rs::start_from_env` rejects tokens unless they are
exactly 64 hexadecimal characters. It still leaves `TAURI_BLITZ_DRIVER_TOKEN` in the
process environment, so subsequently spawned child processes inherit it. Decide where the
host bootstraps and scrubs this value before adding environment mutation;
`start_from_env` has no current in-repository caller, and its API may be invoked after other
threads start. Preserve the 64-hex validation and avoid claiming the token is removed from
the environment.

## Guest fetch boundary

`PlatformHost` allows only same-origin HTTP(S) guest requests, rejects a final redirect to
another origin, and strips `Set-Cookie` and `Set-Cookie2` from exposed responses. This is
intentionally stricter than CORS and fail-closed, but it does not implement the Fetch CORS
protocol or eliminate network side effects of redirects handled by the provider. Keep the
API boundary honest until a complete policy is selected and covered.

## Crypto policy gate

The manual CI and publish workflows now invoke `cargo deny --all-features --config
deny-crypto.toml check bans` before build/publication. `cargo-deny` is not installed in
the current local environment, so this gate has not been executed here. The policy file
explicitly denies RSA and the current `packages/blitz-net/Cargo.toml` enables the
`ps-rustcrypto` `rsa-pss` feature; resolve the existing `POL-002` ruling and run this
focused gate locally before treating the manual workflows or a registry release as ready.

## HTTP disk-cache storage budget

`packages/blitz-net/src/cache.rs` bounds each cached body by `MAX_BODY_BYTES`, but does not
bound the sum of body files or number of cache rows. A long-running profile can therefore
grow the cache without limit. Add a configurable or documented per-profile byte quota and
an eviction policy that accounts for existing files on open, concurrent insertions, row
replacement, failed writes, and stale orphan cleanup. Keep body-file deletion ordered after
metadata publication/removal so crashes cannot leave a live row pointing at a deleted body.
Verify quota enforcement and recovery with cache tests before treating disk use as bounded.

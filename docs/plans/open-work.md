# Open work

This file tracks requirements that remain open; verify each against the current code before changing behavior.

## DOM child-list clone optimization

Revisit the historical child-list clone optimization only after establishing that recursive cloning cannot mutate the source child list. Measure the affected clone path before claiming a performance improvement. Preserve current behavior until correctness and performance are demonstrated.

## Runtime-free network proposal safeguards

The archived proposal is not an instruction to activate the historical transport. If reconsidered, resolve its cache privacy and size limits, response-header/framing correctness, HTTP/2 and proxy/compression behavior, and regression coverage against the actual consumer. See [the existing proposal](../proposals/runtime-free-network.md).

## Package and API release review

This PR removes the public `ScriptDocument` and `ScriptBackend` APIs and replaces the concrete `ApplicationDocument` with a generic backend type. That is a breaking change for consumers of the publishable `ps-blitz-script` package. The PR is source integration, not a release, so it retains the current workspace version `0.6.5`. Before publishing this API, bump the workspace version and affected published packages to `0.7.0` and update their internal version requirements; pre-1.0 semver uses the minor component for this compatibility break. Do not publish this source at `0.6.5`.

No matching `0.6.5` Git tag or GitHub release was found. `cargo info --registry crates-io ps-blitz-script@0.6.5` confirms that `0.6.5` is published with the old Boa API. Therefore the breaking replacement must not be published as `0.6.5`; bump the workspace and affected published packages to `0.7.0` before the release. This PR only integrates source and keeps the current workspace version unchanged.

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
explicitly denies RSA and the current `packages/blitz-net/Cargo.toml:55` enables the
`ps-rustcrypto` `rsa-pss` feature; resolve the existing `POL-002` ruling and run this
focused gate locally before treating the manual workflows or a registry release as ready.

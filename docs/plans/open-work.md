# Open work

This file tracks requirements that remain open; verify each against the current code before changing behavior.

## DOM child-list clone optimization

Revisit the historical child-list clone optimization only after establishing that recursive cloning cannot mutate the source child list. Measure the affected clone path before claiming a performance improvement. Preserve current behavior until correctness and performance are demonstrated.

## Runtime-free network proposal safeguards

The archived proposal is not an instruction to activate the historical transport. If reconsidered, resolve its cache privacy and size limits, response-header/framing correctness, HTTP/2 and proxy/compression behavior, and regression coverage against the actual consumer. See [the existing proposal](../proposals/runtime-free-network.md).

## Package and API release review

This PR removes the public `ScriptDocument` and `ScriptBackend` APIs and replaces the concrete `ApplicationDocument` with a generic backend type. That is a breaking change for consumers of the publishable `ps-blitz-script` package. The PR is source integration, not a release, so it retains the current workspace version `0.6.5`. Before publishing this API, bump the workspace version and affected published packages to `0.7.0` and update their internal version requirements; pre-1.0 semver uses the minor component for this compatibility break. Do not publish this source at `0.6.5`.

No matching `0.6.5` Git tag or GitHub release was found during PR preparation. The registry's published-version status was not queried; verify registry availability during release preparation.

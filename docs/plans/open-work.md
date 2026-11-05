# Open work

This file tracks requirements that remain open; verify each against the current code before changing behavior.

## DOM child-list clone optimization

Revisit the historical child-list clone optimization only after establishing that recursive cloning cannot mutate the source child list. Measure the affected clone path before claiming a performance improvement. Preserve current behavior until correctness and performance are demonstrated.

## Runtime-free network proposal safeguards

The archived proposal is not an instruction to activate the historical transport. If reconsidered, resolve its cache privacy and size limits, response-header/framing correctness, HTTP/2 and proxy/compression behavior, and regression coverage against the actual consumer. See [the existing proposal](../proposals/runtime-free-network.md).

## Package and API release review

For a release that changes a published package, review package versions, public API compatibility, dependent version constraints, workspace lockfile effects, and publication policy together. A change does not imply a version bump by itself; document why no bump is needed when the change is internal-only.

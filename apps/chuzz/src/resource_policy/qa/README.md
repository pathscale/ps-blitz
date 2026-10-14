# Resource policy functional witnesses

The Bun runner serves real pages, launches the shipped headless entry point
through ps-qa, and checks the painted user-visible result. It never builds or
modifies either repository. There are no unit tests or production fault flags.

The file probe uses a real existing canary written in the runner's private
temporary directory. A native file navigation is the positive control. The
private-address page attempts both 127.0.0.1 and a public-looking name that
resolves to the same server, exercising resolved-address enforcement. Cookie
plumbing seeds a real HttpOnly cookie on a native redirect, then checks outgoing
credentials and incoming Set-Cookie on two distinct origins. Request and byte
probes use real HTTP responses and native application limits.

Set QA_RESOURCE_PUBLIC_HOST to a DNS name that the witness environment maps
exclusively to 127.0.0.1. It must not be a localhost or .local name or an IP
literal. Supply that mapping in the environment before running this witness;
the runner checks it but does not edit DNS configuration or hosts files. The
public name is deliberately a public principal even though the browser's
explicit top-level navigation reaches the local fixture. Subsequent requests
must be refused at the resolved-address gate.

Set QA_HOST_BIN to the ordinary patched chuzz-headless executable and PS_QA_BIN
to the compatible ps-qa executable. Supply all five prebuilt mutant executables
using the environment variable names in defects.json. Each mutant contains
exactly its listed native policy edits on top of the patched source. The owner
prepares these binaries outside the read-only patch lane. No server response,
page assertion, CORS failure or missing file substitutes for a policy defect.

Run: bun scripts/qa-resource-policy.ts

The runner requires the native file positive control and every normal rule to
pass. It then runs each mutant against the same served page and unchanged
check file, requires ps-qa exit 1, requires exactly the named rule to fail, and
requires the page-start check to pass. It also observes the application warning
thresholds on the normal cap runs. Missing mutants, incomplete reports,
timeouts and unrelated failures fail the runner.

These artifacts have not been compiled or driven in the patch lane. That lane
permits only source inspection and Bun patch applicability checks.

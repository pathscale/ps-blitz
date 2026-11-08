# Runtime-free network proposal archive

This proposal targeted the historical HTTP/1.1-only `ps-blitz-net` implementation at source
base `5fd4633ccc4d53351b50ca5685761b5c436d4001` and source head
`de471e45650681509aea8fd871019981c7ce9be3`. Its patch archive has been removed because it
replaced current transport, TLS, and cache behavior wholesale and is not a safe implementation
source for this branch.

The current client already provides native TLS policy, HTTP/2 and HTTP/1.1, cookie integration,
compression support, and optional profile-scoped disk caching. The open cache issue is narrower:
body files have a per-entry limit, but the cache has no aggregate storage quota or eviction policy.
Track that requirement in [the open-work plan](../plans/open-work.md#http-disk-cache-storage-budget).

Do not apply the historical patch. Re-review concrete requirements against the current transport
and consumer before proposing code changes.

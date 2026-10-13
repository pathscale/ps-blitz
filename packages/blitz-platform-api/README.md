# `blitz-platform-api`

The host platform APIs a guest expects to find, written once: `fetch`, and
per-origin storage.

This is the third crate on the split that produced `blitz-dom-api`. That one
took the DOM operations and removed the runtime. This one takes the platform
APIs and removes **both** the runtime and the transport, so what is left is the
part every binding would otherwise write for itself.

```text
  blitz-traits         FetchProvider, StorageProvider, OriginKey
        |                  the embedder implements these
  blitz-platform-api   this crate: origin scoping, the in-flight table,
        |              the completion queue, the counters
  blitz-wasm           binds it for a WebAssembly guest
  blitz-script         can bind the same thing for JavaScript

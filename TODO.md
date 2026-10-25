# TODO

Audited 2026-10-14 against `307b2838`. This is the repository's durable open-work list; verification used source and git history, without builds or test runs.

## Open

(None)

## Done

- `separate_process_controls_solid_without_fixed_sleeps` in `packages/blitz-script/tests/debug_control.rs` fixed and verified: supplies `TAURI_BLITZ_DRIVER_TOKEN`, resolves pending layout before `perform_actions` hit-testing in `packages/blitz-script/src/debug_control.rs`, and pins `babel-preset-solid` to `next` in `examples/solid/package.json` so the Solid 2.0 runtime and compiler match (`_$$click`).
- Hardened `blitz-script` (`FileReader` / `fetch("file://...")` deny-by-default and clipboard permission callback), `blitz-dom` (HTML/CSS/layout recursion depth guards, iframe depth & scheme checks, data URI limit, SVG `usvg` fontdb removal), and `blitz-net` (concurrent load semaphore, cache header CRLF sanitization, `Content-Length` pre-check).

- Detached row reclamation and wrapper-cache pruning landed in `47d0b3a8`, enabled by weak caches in `4f434d4b`; lives in `packages/blitz-script/src/dom/mod.rs` and `src/state.rs`; start with `remove_and_free_node`/`sweep_detached_nodes` and `tests/removed_nodes_are_freed.rs` (now 2,000 churned rows, under 40 nodes, plus retained-node usability; current test results unverified).
- Detached listener subtrees, removal-path coverage, and retained descendant listeners landed in `8cbf8255`, with fresh-detachment-only forced GC in `f9164de5`; lives in `packages/blitz-script/src/dom/mod.rs`, `src/dom/node.rs`, and `tests/removed_nodes_are_freed.rs`; start with the listener-cycle and reinsertion tests (current test results unverified); the stale leak handover is folded here and removed.
- The ekojit native page backend moved out in `307b2838` after extraction in `54bfa21a`, and into the ekojit repository's `crates/blitz-script-ekojit` in `fe775269`; ps-blitz retains the engine/document integration seams and Boa implementation; start with `packages/blitz-script/src/engine.rs`, `src/application_document.rs`, and the destination crate's `src/native.rs`/`src/native_document.rs`.
- The four std mpsc channel areas were removed on 2026-10-14: DOM `319dfed8`, shell `cf6317f7`, WGPU example `23b58e79`, debug control `3588896c`; no std mpsc use remains in tracked Rust source, and `clippy.toml` bans `std::sync::mpsc::Sender`; start with `packages/blitz-traits/src/handoff.rs` and `packages/blitz-debug-control/src/handoff.rs` (remaining mpsc calls in `apps/browser/src/url_suggestions.rs` use Tokio).

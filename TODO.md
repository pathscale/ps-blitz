# TODO

Audited 2026-10-14 against `307b2838`. This is the repository's durable open-work list; verification used source and git history, without builds or test runs.

## Open

- The debug harness test still expects `descriptor["token"]` and does not supply `TAURI_BLITZ_DRIVER_TOKEN`, although `91804d02` removed the descriptor token and requires a supplied token; lives in `packages/blitz-script/tests/debug_control.rs`; start with its child-process setup and `DebugController::start_from_env` in `packages/blitz-script/src/debug_control.rs`, supplying a test token and keeping discovery token-free.
- `separate_process_controls_solid_without_fixed_sleeps` reportedly reads counter text `0` instead of `1` after channel-removal history `650f7cdb..e2b991cb` and engine seam `7af7e5ae` (runtime result and causal attribution unverified; the `1` assertion is present); lives in `packages/blitz-script/tests/debug_control.rs`; start after the token setup fix with `DebugController::click`/`wait_for_idle` in `packages/blitz-script/src/debug_control.rs`, event dispatch and polling in `packages/blitz-script/src/document.rs`, delegation in `packages/blitz-script/src/engine.rs`, and `packages/blitz-traits/src/handoff.rs`.

## Done

- Detached row reclamation and wrapper-cache pruning landed in `47d0b3a8`, enabled by weak caches in `4f434d4b`; lives in `packages/blitz-script/src/dom/mod.rs` and `src/state.rs`; start with `remove_and_free_node`/`sweep_detached_nodes` and `tests/removed_nodes_are_freed.rs` (now 2,000 churned rows, under 40 nodes, plus retained-node usability; current test results unverified).
- Detached listener subtrees, removal-path coverage, and retained descendant listeners landed in `8cbf8255`, with fresh-detachment-only forced GC in `f9164de5`; lives in `packages/blitz-script/src/dom/mod.rs`, `src/dom/node.rs`, and `tests/removed_nodes_are_freed.rs`; start with the listener-cycle and reinsertion tests (current test results unverified); the stale leak handover is folded here and removed.
- The ekojit native page backend moved out in `307b2838` after extraction in `54bfa21a`, and into the ekojit repository's `crates/blitz-script-ekojit` in `fe775269`; ps-blitz retains the engine/document integration seams and Boa implementation; start with `packages/blitz-script/src/engine.rs`, `src/application_document.rs`, and the destination crate's `src/native.rs`/`src/native_document.rs`.
- The four std mpsc channel areas were removed on 2026-10-14: DOM `319dfed8`, shell `cf6317f7`, WGPU example `23b58e79`, debug control `3588896c`; no std mpsc use remains in tracked Rust source, and `clippy.toml` bans `std::sync::mpsc::Sender`; start with `packages/blitz-traits/src/handoff.rs` and `packages/blitz-debug-control/src/handoff.rs` (remaining mpsc calls in `apps/browser/src/url_suggestions.rs` use Tokio).

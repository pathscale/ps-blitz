# mega-blitz

**This is not a fork of Blitz. It is an assembly of one.**

Branch `mega-blitz`, a worktree of `~/code/blitz-rust`. Built on top of
`DioxusLabs/blitz` `main`, it collects work that is currently scattered across a merged
history, an unmerged draft PR, a stalled branch, and a private fork, and puts it in one
tree that compiles and passes its tests.

## What it is a collection of

| Source | What it contributes | State here |
|---|---|---|
| **`DioxusLabs/blitz` `main`** | The base. The WPT runner and its CI, `blitz-test-harness`, the SlotMap node tree with versioned `NodeId`, `ThinVec` node lists, `ElementData` boxed off `NodeData`, `<iframe>`, Taffy 0.13, Stylo 0.20, and five crash fixes | **In.** The base commit |
| **`DioxusLabs/blitz` PR #491, `js-engine`** | `blitz-script`, the Boa-backed JavaScript engine and DOM bindings. Nico Burns's, open as a **draft** since 2026-07-05, 8 commits, +5,511 lines. **Not merged to `main`** | **Partly in.** Our fork's copy is here, ported to the new node tree. The PR's own later commits are not yet |
| **`pathscale/ps-blitz`** | Where this branch pushes. 50 commits on top of PR #491: the script surface pages actually hit (`closest`, `matches`, `composedPath`, constructed events, scroll metrics, performance timing), inline SVG fixes, macOS clipboard and delete-key handling, pointer capture, frame timing diagnostics, and a fix for `absolute_position` that upstream still has wrong | **Partly in.** `blitz-script` and two correctness fixes; the rest is step 3 |
| **`DioxusLabs/blitz` PR #549** | `position: fixed` resolving against the viewport rather than a positioned ancestor. The defect recorded as failure 2 in `chuzz/docs/HANDOVER-24x-rendering.md` | **In.** Cherry-picked, 4 tests pass |
| **`DioxusLabs/blitz` PR #578** | The same defect, done more thoroughly and more invasively | **Queued**, after #549 settles |
| **`DioxusLabs/blitz` branch `devin/1782520416-shadow-dom-custom-elements`** | Shadow DOM and custom elements in `blitz-dom`. One commit, 205 behind `main`. This is the wall pathscale.com hits: `customElements is not defined`, backed by `todo!("Shadow roots not implemented")` | **Queued** |
| **Other forks** | `Klemen2`'s double-redraw-on-resize, `UMCEKO`'s `display: contents` and overlay scrollbars | **Queued, low priority** |

**Direction of travel.** We pull from upstream and push to `pathscale/ps-blitz`. Nothing
goes back the other way, so a fix we find in upstream's code is ours to carry.

## Why it exists

Because the pieces a working browser needs are in four places and none of them has all of
them. `main` has the test infrastructure and the node tree; the JavaScript engine is on an
unmerged draft; the DOM APIs real pages call are on a private fork; shadow DOM is on a
branch nobody has rebased in 205 commits.

Our own fork was 59 commits divergent and 74 behind, which meant re-doing upstream's work
by accident: we arrived at Taffy 0.13 and Stylo 0.20 independently, and found out
afterwards.

## What is done

Three commits on top of `main`, `cargo test --workspace` green at **273 passing, 0
failing**:

1. `blitz-script` ported onto the new node tree. 8,405 lines, 138 compile errors to zero.
   Node ids are versioned, `NodeData::Document` and `::Comment` changed shape, comments
   store their contents, children are a `ThinVec`, and `scroll_offset` and `final_layout`
   moved behind accessors.
2. Document bubbling restored to `node_chain`. Events bubble to the Document after the
   last element, so the document node belongs on the propagation chain even though the
   ancestor filter drops it.
3. `absolute_position` corrected. Upstream subtracts a node's **own** scroll offset from
   its **own** border box; a scroll offset moves descendants, not the scroller. A scroll
   container with `scrollLeft = 80` reported its left edge as `8 - 80`. Upstream still has
   this bug; we do not send patches back, so it stays ours.

Points 2 and 3 were both caught by `blitz-script`'s tests, and neither would have survived
a mechanical rebase.

## The plan

Order and reasoning in `chuzz/docs/mega-blitz.md`. In short: finish replaying the fork,
take the queued PRs, then the node-tree work nobody has done, which is splitting layout
off the node. A node is 1,600 bytes and two thirds of elements on a content page have no
layout box.

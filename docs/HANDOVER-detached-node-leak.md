# Root-cause prompt: detached DOM nodes are never freed

## The bug, in one sentence

`removeChild` detaches a node instead of dropping it, and nothing ever drops it
afterwards, so every node a framework removes stays in the document for the rest
of the session.

## Evidence

Measured on a running AgencyZero instance after a few hours of ordinary use:

```
nodes=98646 inspect_ms=720.6
generic=82396  button=6023  presentation=4342
```

A freshly launched window of the same application holds **635** nodes and
inspects in **4.5ms**. That is 155x the tree and 160x the inspection cost.

Of a 3000-node sample, **2728 had no box at all** — 91% of the document is
detached subtrees nobody can see. The duplicates are unmistakable:

```
4294974075,button,Show the whole command,945,846,0,0,"hidden,0x0"
4294974102,button,Show the whole command,945,846,0,0,"hidden,0x0"
4294974126,button,Show the whole command,945,846,0,0,"hidden,0x0"
   ... 1073 more, sequential ids ~24 apart, identical coordinates
```

One abandoned subtree per list row that scrolled out of the window.

`ps-qa ghost`, which exists precisely to report hidden nodes owning boxes,
**cannot complete against it** — the inspector drops the connection before it
finishes serving a tree that size.

## Where it is

`packages/blitz-dom/src/mutator.rs`, `Mutator::remove_node`:

```rust
// A detached node keeps its box otherwise ... The node is
// deliberately not dropped, so JS wrappers stay valid, but nothing
// outside the document should occupy space in it.
Self::clear_layout_of_subtree(self.doc, node_id);
```

The tradeoff is deliberate and half-finished: layout is cleared, the node is
kept, and **nothing ever frees it**. `remove_and_drop_node` sits directly below
and is never called from the JS bindings. Every removal path in
`packages/blitz-script/src/dom/` calls `remove_node`:

```
element.rs:905, node.rs:260, node.rs:315, node.rs:359,
node.rs:381, node.rs:404, node.rs:434, node.rs:471
```

`node.rs:315` is `appendChild` reparenting and must stay a detach. The rest
discard children and leak.

## Why it surfaced now

The leak is old. `git log -S "so JS wrappers stay valid"` dates the
detach-not-drop behaviour to before 2026-08-12, when
`fix(dom): a detached node must not keep its layout box` added the layout
clearing but left the node.

What changed is the *rate*. AgencyZero's task log adopted `createFlexGrid` on
**2026-08-22** (`5d9c914 refactor: take the shared pager from @pathscale/ui`).
Before that the log rendered every loaded entry, so `<For>` appended and rarely
removed. After, it renders a 20-row window, so every scroll and every new entry
*removes* rows. Same bug, from near-zero churn to one leaked subtree per row.

FlexGrid is not at fault. `createFlexGrid` is `createMemo` + `slice`, and the
consuming code is `<For each={entries()}>`. Both are correct. The pager exposed
an engine bug rather than introducing one.

## Why the second half is hard

The comment is right that a node script still references must stay usable. The
naive fix breaks that. Two attempts and why each failed:

**Attempt 1 — "free it if no wrapper exists."** Fails to free anything.
`node_wrapper` (`packages/blitz-script/src/dom/mod.rs`) caches a wrapper for
every node script touches, and `createElement` touches all of them, so every
framework-built node looks reachable for ever.

**Attempt 2 — "free it if only the cache holds the wrapper."** Not expressible.
Boa collects with a tracing GC, not reference counts; there is no
`JsObject::strong_count`.

**Attempt 3 — "free it if it has no event listeners."** Frees the leak correctly
and **breaks the contract**: a node held only by a closure variable, with no
listeners, is freed and then panics at `mutator.rs:129` when script writes to
it. There is a test for exactly this below.

## The root-cause fix

`RuntimeState::node_wrappers` (`packages/blitz-script/src/state.rs:58`) is:

```rust
pub node_wrappers: FxHashMap<NodeId, JsObject>,
```

A **strong** map that is never pruned — `grep -rn "node_wrappers.remove"` across
the crate returns nothing. So the cache alone keeps every wrapper alive, which
is both a wrapper leak in its own right and the reason node reachability cannot
be determined.

The fork already ships what this needs:
`ps-boa-gc-1.0.0-dev/src/pointers/weak.rs` defines `WeakGc`.

**Change `node_wrappers` to hold `WeakGc<...>`.** Then:

- A wrapper script still holds stays alive, and upgrading the weak handle
  succeeds — the node is reachable, so detach it exactly as today.
- A wrapper only the cache held is collected, upgrading yields `None` — nothing
  in script can name the node, so `remove_and_drop_node` it and prune the entry.

This makes Boa's GC the authority on reachability, which it already is, instead
of guessing from listeners or wrapper presence. Apply the same treatment to
`dataset_wrappers`, `class_list_wrappers` and `node_listeners`, all of which are
keyed by `NodeId` and never pruned.

## Tests, already written and committed

`packages/blitz-script/tests/removed_nodes_are_freed.rs` holds both halves:

- `churning_rows_does_not_grow_the_document` — appends and removes 100 rows
  holding no reference. **Currently fails**: `left 210 nodes` (2 per row, none
  freed). Must end under 40.
- `a_removed_node_script_still_holds_stays_usable` — removes a node held by a
  closure variable and writes to it afterwards. **Must keep passing**; the
  listener-based attempt panicked here.

A correct fix turns the first green without regressing the second. Any change
that passes only one of them is one of the failed attempts above.

## Detection, already landed

`ps-qa` now fails any check when the document is mostly invisible, on every
check, with nothing to opt into (`feat/sweep-components`):

```
the document holds 86325 node(s) with no box against 12321 with one,
out of 98646; something is retaining subtrees rather than reusing them
```

Verified firing on the leaking instance and silent on a fresh one. This is why
241 QA checks passed throughout the leak: every one asks about a control it can
name, and abandoned nodes are `0x0` and hidden, so they never perturb what is
being measured. The guard asks about the tree instead.

# Root cause: nodes are never freed, for a reason that stopped being true

## The one-line version

`removeChild` refuses to drop a node "so JS wrappers stay valid" — but `NodeId`
already carries a generation and `SlotMap` already validates it, so a wrapper
holding a dropped node is *already* safe. The precaution outlived the problem it
was written for, and it costs an unbounded document.

## The root root problem

Three layers, each true, and the bottom one is the one to fix:

**Layer 1, the symptom.** A document grows without bound. Measured on a running
AgencyZero instance after a few hours of ordinary use:

```
nodes=98646 inspect_ms=720.6
generic=82396  button=6023  presentation=4342
```

A freshly launched window of the same application holds **635** nodes and
inspects in **4.5ms**: 155x the tree, 160x the cost. Of a 3000-node sample,
**2728 had no box at all** — 91% of the document is detached subtrees nobody can
see. One abandoned subtree per list row that scrolled out:

```
4294974075,button,Show the whole command,945,846,0,0,"hidden,0x0"
4294974102,button,Show the whole command,945,846,0,0,"hidden,0x0"
   ... 1074 more, sequential ids ~24 apart, identical coordinates
```

**Layer 2, the mechanism.** `Mutator::remove_node`
(`packages/blitz-dom/src/mutator.rs:786`) detaches and clears layout but never
frees. `remove_and_drop_node` sits directly below it and is called from **zero**
JS bindings — every removal path in `packages/blitz-script/src/dom/` calls
`remove_node`:

```
element.rs:905, node.rs:260, node.rs:315, node.rs:359,
node.rs:381, node.rs:404, node.rs:434, node.rs:471
```

`node.rs:315` is `appendChild` reparenting and must stay a detach. The rest
discard children and leak.

**Layer 3, the root cause.** The comment explaining why:

```rust
// The node is deliberately not dropped, so JS wrappers stay valid, but
// nothing outside the document should occupy space in it.
```

That is a memory-safety precaution against a dangling `NodeId`. **It is no
longer needed**, because the storage already solves it:

- `packages/blitz-dom/src/tree.rs:33` — `pub struct NodeTree(SlotMap<NodeKey, Node>)`
- `packages/blitz-traits/src/node_id.rs:12` — `pub struct NodeId(u64)`, documented
  as "index + version"

`SlotMap` bumps the version on `remove`, so a stale key never resolves to a
reused slot. `NodeTree::get` returns `None`; `NodeTree::index` panics loudly.
Neither is unsafe, and neither can silently address the wrong node.

So the tradeoff is inverted: the code pays an unbounded leak to avoid a hazard
its own data structure already prevents. **Fixing the root cause means dropping
the node and letting the generation do the job it was designed for.**

## Why it surfaced now

The leak is old. `git log -S "so JS wrappers stay valid"` dates the
detach-not-drop behaviour to before 2026-08-12, when
`fix(dom): a detached node must not keep its layout box` added layout clearing
but left the node.

What changed is the *rate*. AgencyZero's task log adopted `createFlexGrid` on
2026-08-22 (`5d9c914`). Before, it rendered every loaded entry, so `<For>`
appended and rarely removed. After, it renders a 20-row window, so every scroll
and every new entry removes rows. Same bug, near-zero churn to one leaked
subtree per row.

FlexGrid is not at fault. `createFlexGrid` is `createMemo` + `slice`; the
consuming code is `<For each={entries()}>`. Both correct. The pager exposed an
engine bug rather than introducing one.

## The fix

In `packages/blitz-script/src/dom/`, replace `mutr.remove_node(id)` with a drop
at every site that **discards** a child, leaving `node.rs:315` (reparent) alone.

Then make stale access survivable rather than fatal, because dropping turns a
latent stale id into a live one:

1. **Prune the wrapper caches on drop.** `RuntimeState`
   (`packages/blitz-script/src/state.rs:58`) holds `node_wrappers`,
   `dataset_wrappers`, `class_list_wrappers` and `node_listeners`, all keyed by
   `NodeId` and **never pruned** — `grep -rn "node_wrappers.remove"` returns
   nothing. `remove_and_drop_node_with` reports every dropped id; remove those
   entries.

2. **Make a stale wrapper throw, not panic.** `Mutator` indexes raw in 63 places
   (`self.doc.nodes[node_id]`), and `SlotMap`'s `Index` panics on a stale key.
   A script holding a removed node must get a JS exception, not a process abort.
   The checked accessors already exist: `get_node` / `get_node_mut` return
   `Option`. The binding layer should resolve through those and raise a
   `JsNativeError` when the node is gone.

Step 2 is the part that makes step 1 safe, and it is the reason the obvious
one-line fix is not enough.

## Attempts that do not work, and why

Recorded so nobody spends the afternoon I did.

**"Free it if no wrapper exists."** Frees nothing. `node_wrapper`
(`dom/mod.rs:65`) caches a wrapper for every node script touches, and
`createElement` touches all of them, so every framework-built node looks
reachable for ever.

**"Free it if only the cache holds the wrapper."** Not expressible. Boa collects
with a tracing GC, not reference counts; there is no `JsObject::strong_count`.

**"Free it if it has no event listeners."** Frees the leak correctly and
**breaks the contract**: a node held only by a closure variable, with no
listeners, is freed and then panics at `mutator.rs:129` when script writes to
it. The second test below covers exactly this.

**"Hold `WeakGc` in `node_wrappers`."** Tempting, and the fork does ship
`ps-boa-gc/src/pointers/weak.rs`. But it answers a question that no longer needs
asking: with generational ids, a dropped node is *already* safe to reference.
Weak wrappers would still be worth doing to stop the wrapper cache itself
growing without bound, but they are not the root fix.

## Tests, committed and failing

`packages/blitz-script/tests/removed_nodes_are_freed.rs`:

- **`churning_rows_does_not_grow_the_document`** — appends and removes 100 rows
  holding no reference to any. **Fails today**: `left 210 nodes` (2 per row,
  none freed). Must end under 40.
- **`a_removed_node_script_still_holds_stays_usable`** — removes a node held by
  a closure variable, then writes to it. **Passes today and must keep passing.**
  This is the contract the detach exists to honour and what killed the
  listener-based attempt.

A correct fix turns the first green without regressing the second. Any change
that passes only one is an attempt already ruled out above.

Worth adding alongside the fix, once stale access throws rather than panics:

- a test that a script writing to a *dropped* node raises a JS exception rather
  than aborting the process, which is the behaviour step 2 is for.

## Detection, already landed

`ps-qa` now fails any check when the document is mostly invisible, on every
check, with nothing to opt into (branch `feat/sweep-components`):

```
the document holds 86325 node(s) with no box against 12321 with one,
out of 98646; something is retaining subtrees rather than reusing them
```

Verified firing against the leaking instance and silent against a fresh one.

This is why 241 QA checks passed throughout: every one asks about a control it
can name, and abandoned nodes are `0x0` and hidden, so they never perturb what
is measured. `ps-qa ghost`, which exists precisely to report hidden nodes owning
boxes, **could not complete** — the inspector dropped the connection before
serving a tree that size. The guard asks about the tree instead of a node.

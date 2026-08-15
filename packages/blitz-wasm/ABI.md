# The `blitz-wasm` ABI

Every decision the boundary makes, and the reasoning behind each. Numbers in
this document are asserted by `tests/end_to_end.rs`, not quoted from a run: if
one of them changes, that test fails.

## The imports

Eight functions, all in the module named `blitz`.

| Import | Signature | Returns |
| --- | --- | --- |
| `intern` | `(ptr: i32, len: i32) -> i32` | atom, or error |
| `create_element` | `(tag_atom: i32) -> i32` | handle, or error |
| `create_text` | `(ptr: i32, len: i32) -> i32` | handle, or error |
| `append_child` | `(parent: i32, child: i32) -> i32` | `OK`, or error |
| `set_attribute` | `(node: i32, name_atom: i32, value_atom: i32) -> i32` | `OK`, or error |
| `set_text` | `(node: i32, ptr: i32, len: i32) -> i32` | `OK`, or error |
| `add_listener` | `(node: i32, event_atom: i32) -> i32` | listener id, or error |
| `remove_listener` | `(listener_id: i32) -> i32` | `OK`, or error |

Five of these are the operation set the original brief asked for. **`intern` is
the sixth and it is not optional.** Every one of the five takes names as atoms
and nothing else in the ABI produces an atom, so without it the interned tier is
unreachable and the five ops cannot be called at all. It is listed separately
throughout this document because it is also the only place a name is ever
copied, and folding its cost into the operations that consume atoms is exactly
how this measurement would end up overstating itself.

`add_listener` and `remove_listener` are what make the page respond rather than
merely exist. Both are in the interned tier: a handle and an atom, zero bytes.

`set_text` maps onto `blitz_dom_api::node::set_text_content`, which rewrites a
text node in place and replaces an element's children with a single text node.
One import covers both the update case and the "empty this and put a string in
it" case, which is why there is no separate `set_data`.

## The export

One, and it is the whole event path.

| Export | Signature | Returns |
| --- | --- | --- |
| `dispatch` | `(listener_id: u32) -> i32` | `OK`, or the guest's own status |

`alloc` and `dealloc` are also exported by the bindings; see "Deviations"
below, where they are recorded as unused.

**The guest's contract for `dispatch` is that it *completes*.** It must run the
handler **and** leave the guest settled — drain its microtask queue, flush its
scheduler, whatever "settled" means to it — before returning, because the host
takes the document back the instant it returns and may lay it out and paint.

The host does not know what a microtask is, and must not learn. A host that
knew to flush a queue would be a host that knows which framework is on the
other side, and this would stop being an ABI and start being that framework's
ABI. So the drain is named in the contract and unnamed in the signature. In the
demo guest that means `dispatch` lives in the crate that knows — it calls
`blitz_wasm_guest::run_listener` and then `solid_rs::flush()` — rather than in
the framework-neutral bindings.

## Events: deferred dispatch

**The host queues listener ids during propagation and calls the guest only
after the document is released.** This is the single most consequential
decision in the event path, so it is written out in full.

### The problem

`blitz-dom`'s `EventHandler::handle_event` runs *while the `EventDriver` holds
the document.* Calling a guest export from inside it would break the rule in
"Reentrancy" below, and not theoretically: a guest's first act on a click is to
mutate the DOM, which needs the very document the driver is holding.

That rule is enforced by construction here, so "call the guest from the
handler" is not a thing that compiles badly — it is a thing that does not
compile. The choice was to relax the rule or to sidestep it.

### The design

Sidestep it, in four steps:

1. `WasmEventHandler::handle_event` does one thing: pushes the matching
   listener ids onto a pending queue on the host. It calls no guest code and
   returns immediately.
2. The `EventDriver` finishes propagation and default actions, then drops. The
   document borrow ends with it.
3. Only then does `dispatch_dom_event` drain the queue, calling `dispatch` once
   per listener, with the document owned again.
4. A redraw is requested once, after the queue is empty.

The handler is not *trusted* to avoid the guest. It is handed a reference to
the interner, the listener table and the queue, and nothing else — no `Store`,
no `Instance` — so it has no means to reach the guest whatever it intends. Same
technique as `read_string`: make the rule something the compiler checks rather
than something a reviewer remembers.

### What this gives up

**A guest handler cannot `preventDefault` or `stopPropagation`.**

By the time it runs, propagation is over and the default action has already
happened. There is nothing left to prevent or to stop. The ABI therefore does
not offer either, rather than offering them and having them silently do
nothing, which is the worse of the two failures.

This is a real deviation from the DOM, taken knowingly. It is the price of the
reentrancy guarantee, and it is the right price today: the operation set here
builds and mutates a tree, and no default action it can trigger is one a guest
would want to veto. A handler that must veto a default action is future work,
and it is not a small change — it needs the guest called *during* propagation,
which needs the document reachable from inside `handle_event`, which is the
rule this crate is built around. The likely shape is a second, restricted guest
export that may only answer yes or no and may not call back into the host at
all, so that a veto never needs the document. That is a design, not a patch.

Two pieces of listener semantics *are* honoured:

- **A listener removed before it runs does not run.** The queue holds ids, and
  each is re-checked against the table at drain time, so a handler that removes
  a listener queued behind it takes effect.
- **A listener registered by a handler does not run for the event in flight.**
  The queue is taken out of the host before draining, so anything registered
  during the drain lands in the next event's queue.

### Ordering

Listeners fire in bubble order: the target's first, then each ancestor's, and
within one node in registration order. There is no capture phase, because there
is no capture flag in `add_listener` — capture-phase listeners would be the
same walk reversed, and adding the flag before there is a caller to observe
would mean guessing at the semantics.

### Redraw

Requested once after the queue empties, not once per listener: three handlers
responding to one click are one frame's worth of work. It is requested whenever
a listener ran, which is deliberately coarser than `mutated()`. A handler that
changed nothing costs a redundant frame; a handler whose change is never drawn
costs the user a stale screen, and those two are not the same size of mistake.

There is no shell here, so the request is *recorded* — `Host::redraw_requested()`
— rather than sent. See "Obligations this binding inherits".

## Handles

**A node crosses as an opaque `u32`, never as a `NodeId`.**

A `NodeId` is an index into the document's arena. A guest handed raw ids could
address every node in the document by counting from zero, including nodes
belonging to a page it was never given. A handle is an index into *this
instance's* table, and that table only ever contains the mount point the host
seeded plus nodes the guest itself created.

So a forged handle is not an escalation. Either it is out of range, which is
`ERR_BAD_HANDLE`, or it names a node this guest already holds a handle for.
There is nothing to reach that was not already reachable. Every host function
that takes a handle validates it before doing anything else.

**Handle 0 is the mount point**, seeded by the host at `Host::new`. This is not
a convenience: all five operations either create a *detached* node or need one
that already exists, so without a starting handle a guest can build a tree and
has nowhere to put it. The brief's five-operation set has no way to obtain one,
which is why the host supplies it.

**Handles are never reused and never freed.** A node the guest detaches keeps
its handle, matching `blitz-dom-api`'s own detach-not-drop rule: the node stays
addressable, so a handle to it stays meaningful. The cost is that a long-lived
guest's table only grows. That is the right trade for now and the wrong one
eventually; a generational handle is the fix, and it is not needed until a
guest churns nodes in a loop.

## Strings: two tiers

### Tier (a), interned — tag names, attribute names, attribute values

These cross once, as UTF-8 bytes, through `intern`, and are an `AtomId` `u32`
thereafter. The guest bindings memoise on their side too, so a guest calling
`set_attribute(node, "class", "row")` in a loop performs **one** `intern` per
distinct string for the whole life of the instance, not one per iteration.

Interning a *value* is right for a class list, an id, or an enum-like attribute,
and wrong for a value drawn from an unbounded set: an atom is never released, so
interning per-frame text would grow the host's table without bound. The guest
bindings say so at `Element::set_attribute`, and `Node::set_text` is the
alternative for content.

### Tier (b), copied — text content

`create_text` and `set_text` take `(ptr, len)` into guest linear memory and the
host reads it. Text is the content of a page, not a name from a small fixed
vocabulary; interning it would grow the table without bound and save nothing,
because the second occurrence of a given sentence is rare.

### Which operation uses which

| Operation | Tier | Bytes copied per call |
| --- | --- | --- |
| `intern` | (b), by definition | `len` |
| `create_element` | (a) | 0 |
| `create_text` | (b) | `len` |
| `append_child` | neither: two handles | 0 |
| `set_attribute` | (a) for both name and value | 0 |
| `set_text` | (b) | `len` |
| `add_listener` | (a) | 0 |
| `remove_listener` | neither: one listener id | 0 |
| `dispatch` (export) | neither: one listener id | 0 |

### The measurement: mounting

From `tests/end_to_end.rs`, building this page:

```html
<div class="counter">
  <button class="increment">+1</button>
  <span class="count">0</span>
</div>
```

| Operation | Calls | Bytes copied |
| --- | --- | --- |
| `intern` | 8 | 44 |
| `create_element` | 3 | 0 |
| `create_text` | 2 | 2 |
| `append_child` | 5 | 0 |
| `set_attribute` | 3 | 0 |
| `set_text` | 1 | 1 |
| `add_listener` | 1 | 0 |
| **total** | **23** | **47** |
| **total excluding interning** | | **3** |

The 44 bytes are the eight distinct names: `div`, `class`, `counter`,
`button`, `increment`, `span`, `count`, `click`. Every one crosses exactly
once, no matter how many elements or listeners later use it. The 3 are the
content: `+1` on the button, and the `0` the effect wrote into a text node
that was created empty.

**Read the two totals together.** 3 bytes is what this page costs once the
vocabulary is known, which is the steady state a running page is in. 47 is what
the first paint costs including learning the vocabulary. Quoting only the first
would be a true number telling a false story, which is why
`Counters::total_bytes_copied` and
`Counters::bytes_copied_excluding_interning` both exist and the test asserts
both.

### The measurement: clicking

This is the number the event path exists to produce.

| | Calls | Bytes copied |
| --- | --- | --- |
| `dispatch` | 1 | **0** |
| `set_text` | 1 | 1 |
| everything else | 0 | 0 |

**A click copies zero bytes.** An event is a listener id and nothing else —
there is no pointer in `dispatch`'s signature for anything else to travel
through, so this is a structural zero rather than a measured one. The single
byte is the digit the effect produced, which is the only genuinely new
information the click created.

Ten clicks cost eleven bytes: nine single digits and one `10`. Nothing is
interned, no name crosses again, and the per-click cost does not grow with the
tree, the number of listeners, or the number of clicks.

The demo module is 117,450 bytes built `--release`, of which roughly 97 KB is
`solid_rs` — `SolidRS/crates/solidrs-wasm-smoke` measures the reactive core
alone at about 113 KiB. Module size is not what this crate optimises: the guest
bindings use `std` rather than `no_std` because a `no_std` wasm32 guest has no
global allocator and no panic handler, and supplying both by hand would shrink
the module without moving a single boundary byte.

The number worth noticing is the other one: **a guest carrying a whole reactive
framework imports the same eight names as a guest carrying none.** The
framework is entirely on the guest side of the boundary, which is what
`the_guest_imports_only_the_blitz_module` asserts.

## Errors

**Every host function returns `i32`. Negative is an error, non-negative is
success.** For an operation that creates something the value *is* the handle or
atom; for the rest it is `OK`, which is zero. Handles and atoms are therefore
capped at `i32::MAX`, which buys a single return value instead of an
out-pointer and the bounds check that out-pointer would need.

| Code | Meaning |
| --- | --- |
| `0` | `OK` |
| `-1` | `ERR_BAD_HANDLE` |
| `-2` | `ERR_BAD_ATOM` |
| `-3` | `ERR_BAD_MEMORY` |
| `-4` | `ERR_BAD_UTF8` |
| `-5` | `ERR_DOM` |
| `-6` | `ERR_TOO_MANY_HANDLES` |
| `-7` | `ERR_BAD_LISTENER` |
| `-8` | `ERR_TOO_MANY_LISTENERS` |

`ERR_BAD_LISTENER` is separate from `ERR_BAD_HANDLE` because they are different
namespaces: a listener id indexes the listener table, a handle indexes the node
table, and a guest that passes one where the other belongs should be told which
one it got wrong rather than hitting an unrelated node. Listener ids are never
reused, for the same reason handles are not — a stale id must be an error, not
a silent hit on whatever took its place.

The guest's `dispatch` return value follows the same convention but is **not**
in this table: a guest's status codes are its own, and it is free to mean
anything by `-3`. The host keeps the last negative one in
`Counters::last_guest_status` purely so a failing test can say which listener
reported trouble instead of "a click did nothing".

**Nothing traps on a guest mistake.** A trap tears down the instance and takes
the reason with it, so a guest that passed a bad handle would learn only that
it died. `a_forged_handle_is_an_error_not_a_trap` asserts that the instance is
still usable after a rejected call.

Every `DomError` collapses into the single `ERR_DOM`, because a guest cannot
act differently on `TreeInvariant` than on `NodeNotFound` and a stable ABI is
worth more than a taxonomy nobody branches on. The detail is not discarded:
`Counters::last_dom_error` holds the rendered error so a failing test says
something better than "the guest got -5".

## Reentrancy

**THE RULE: no document borrow is held across a call into the guest.**

Enforced by construction, not by memory. `Host` *owns* the `BaseDocument`, and
a host function reaches it only through `Caller::data_mut` for the duration of
its own body. There is nowhere to put a `&mut BaseDocument` that outlives a
call because this crate never stores one.

`read_string` is where the rule is visible in the code: it borrows guest
memory, copies out, and drops that borrow *before* the document is touched.
Written the other way round it does not compile, which is the property worth
having.

**Event dispatch is where the rule was tested and where it held.** The one
place in this crate that genuinely wants to call the guest with the document
borrowed is `EventHandler::handle_event`, and it does not: it queues ids and
returns, and the guest is called afterwards from `dispatch_dom_event`, with the
borrow already gone. The handler could not do otherwise — it is constructed
with three field references and no `Store`, so there is no path from it to a
guest export. See "Events: deferred dispatch" for the full design and for the
`preventDefault` limitation it costs.

## Counters

Three numbers per operation, and deliberately no timing. A timing number
measured on one machine, in one build profile, against an interpreter is not
evidence; a byte count is identical everywhere and is exactly what the boundary
design changes.

`bytes_copied` counts bytes read out of guest linear memory, and nothing else.

`dispatch` is counted here too, and it is the one counter that goes the other
way: a call the *host* made into the guest. It is in this table anyway because
it is the number that answers "what does a click cost at the boundary", which
is the question the whole event path exists to answer. It is excluded from
`total_calls`, which counts inbound calls and would answer no question at all
with an outbound one added to it.

`host_allocs` counts allocations **this crate** makes: the `String` built from
guest memory. It does not count allocations inside `blitz-dom-api` or
`blitz-dom`, which this crate cannot see without instrumenting packages it does
not own, and it does not count the amortised growth of the listener table on
`add_listener`, which is not a per-call allocation. `add_listener`'s zero
therefore means "no string was built", not "no memory moved". Those omitted
allocations exist and are not negligible:
`blitz_dom_api::document::create_element` lowercases the tag into a fresh
`String`, and every reader in the facade returns an owned `String` by design
(see its MAPPING.md, "Readers allocate a `String`, so the wasm path pays two
copies"). A reader of these counters must not take "zero host allocs" to mean
"nothing was allocated".

Within that definition, `set_attribute` and `create_element` really do allocate
nothing here. That took work: the obvious implementation resolves an atom and
calls `.to_owned()` on it, because the interner and the document are both
fields of `Host` and reaching through `host.` for both does not borrow-check.
Destructuring `Host` into disjoint fields lets the interned name be *borrowed*
into the facade instead. Without that the counter would have read zero while
two `String`s were allocated per call, which is the sort of true-but-misleading
number this file exists to prevent.

## Obligations this binding inherits

`blitz-dom-api` deliberately leaves three things to its caller, and this crate
is the caller. See its MAPPING.md.

1. **Layout dirtiness.** The facade never marks layout dirty.
   `Host::mutated()` is this binding's flag, set by every mutating operation.
   An embedder reads it to decide whether to resolve layout and ask for a
   frame, then calls `clear_mutated`.
2. **Redraw requests.** There is no shell here, so there is nothing to ask.
   `dispatch_dom_event` records the request on `Host::redraw_requested()`
   instead, once per event that ran a listener; an embedder with a shell reads
   it, asks its window for the frame, and calls `clear_redraw_request`. An
   embedder driving the guest's exports directly, with no events involved,
   gates on `mutated()` as before.
3. **The layout flush before a geometry read.** None of the eight operations
   reads geometry, so this does not bite yet. It will the moment
   `getBoundingClientRect` is added: the facade does not flush, and a caller
   that forgets reads the layout from before its own mutations, silently.

A fourth, for when `set_inner_html` is added: **the host must install a real
HTML parser provider** (`blitz-html`). The default
`DummyHtmlParserProvider` parses nothing, so `set_inner_html` against a default
document succeeds and silently empties the element.

## Deviations from the brief, and why

- **`intern` is a sixth import.** Without it the atom tier has no producer.
- **The host seeds handle 0.** Without it a guest has nothing to attach to.
- **The guest is a separate, excluded workspace,** so the workspace change is
  one `members` line *and* one `exclude` line rather than one line. Two
  reasons: `cargo test -p blitz-wasm` holds the lock on this workspace's target
  directory, so a guest sharing it would deadlock rather than fail, and a
  deadlocked test looks like a hung machine; and the guest only builds for
  `wasm32-unknown-unknown`, so keeping it out of `members` keeps
  `cargo check --workspace --all-targets` from trying to build a cdylib for the
  host triple. Same shape as `SolidRS/crates/solidrs-wasm-smoke`, where the
  pattern was proven. Its `Cargo.lock` is committed for the same reason that
  one is: the harness reproduces a measurement, and a measurement whose
  versions cannot be re-resolved is an anecdote.
- **`alloc`/`dealloc` are exported by the guest but the host never calls
  them.** With these eight operations every string travels guest to host, so
  the guest allocates its own and the host only reads. They exist now because
  the first operation that returns a string to the guest needs them to already
  have a settled signature.
- **`dispatch` is exported by the demo guest, not by the bindings.** The
  bindings supply `run_listener`, which is half of it. The other half is the
  drain, and only the guest knows what its framework considers settled; a
  bindings crate that called `solid_rs::flush()` would make this Solid's ABI.
  See "The export".
- **A guest handler cannot cancel an event.** See "Events: deferred dispatch",
  "What this gives up". This is the one place the ABI knowingly does less than
  the DOM.
- **`add_listener` returns a listener id, not a handle.** Two namespaces rather
  than one, which is one more thing for a guest to keep straight. The
  alternative is `remove_listener(node, event)`, which cannot remove one of two
  listeners registered on the same node for the same event without also
  identifying *which*, and identifying which needs an id anyway.
- **The demo guest depends on `solid_rs` by git rev, not by path.** A
  `../../../../../SolidRS` resolves on the machine it was written on and
  nowhere else. The rev is pinned and `guest/Cargo.lock` is committed, so the
  measurement above can be reproduced rather than merely repeated. The first
  build needs network access to fetch it; every later one does not.
- **`set_attribute` has no copied-value variant.** Both name and value are
  atoms. A `class` computed per frame therefore cannot be set without interning
  it, which is the wrong trade for an unbounded value set. The fix is a
  `set_attribute_str` taking `(ptr, len)` for the value; it is not needed for
  the five-operation set and adding it now would mean guessing at the split
  before there is a caller to observe.

## Not what the brief assumed

`blitz-script` does not yet bind `blitz-dom-api`; reparenting it is a separate,
unstarted change. So this crate is not the facade's second consumer, it is its
**first**. Anything awkward recorded above is therefore evidence about the
facade rather than about this binding, and the two entries under "Known costs"
in the facade's MAPPING.md were written from this crate's needs.

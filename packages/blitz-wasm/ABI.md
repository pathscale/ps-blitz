# The `blitz-wasm` ABI

Every decision the boundary makes, and the reasoning behind each. Numbers in
this document are asserted by `tests/end_to_end.rs`, not quoted from a run: if
one of them changes, that test fails.

## The imports

Eleven functions, all in the module named `blitz`.

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
| `get_attribute` | `(node: i32, name_atom: i32, out_ptr: i32, out_cap: i32) -> i32` | byte length, `ABSENT`, or error |
| `text_content` | `(node: i32, out_ptr: i32, out_cap: i32) -> i32` | byte length, or error |
| `has_attribute` | `(node: i32, name_atom: i32) -> i32` | `0`, `1`, or error |

The first eight all go one way: the guest owns the bytes and the host reads
them. The last three go the other way, and that reversal needed a mechanism
that did not exist — see "The read direction".

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

## The read direction

**This is the axis the design does not win, and this section exists to say so
with numbers rather than to hedge.**

The prediction was that reads would reverse the result, for two reasons. First,
every reader in `blitz-dom-api` returned an owned `String`, so a read cost two
copies: one into that host `String` and one into guest memory. Second, the
handle-and-atom design saves nothing on a read, because the bytes coming back
*are* the payload — there is no vocabulary to amortise when the answer is the
content.

Both held on first measurement. **The first has since been removed and the
second has not**, because only one of them was ever about the boundary. See
"The second experiment" below for what changed and by how much; the numbers
under "The measurement: reading" are the current ones, and the ones they
replaced are quoted beside them.

### The mechanism: no direction existed to reuse

Every string in the first eight operations travels guest to host as
`(ptr, len)`, with the guest owning the memory. A read goes the other way, and
nothing in the ABI went that way before. Three mechanisms were available.

**(a) Two calls.** Ask for the length, allocate, ask again for the bytes.
*Failure mode: the value can change between the two calls.* Nothing in this
crate can currently change it — the guest is single-threaded and the host is not
running — but the ABI would be promising something it does not enforce, and the
first embedder that mutates the document from a host-side widget between the two
calls gets a truncated or over-read value with no error. It also costs two
crossings for **every** read, not just the ones that do not fit.

**(b) The guest supplies the buffer.** `(out_ptr, out_cap)` in, the value's full
byte length out, and the bytes written only if they fit. **Chosen.**
*Failure mode: cost, not correctness.* A value longer than the buffer costs a
second host call and a second host-side allocation of the whole value, and the
guest sees the same bytes either way. Nothing is ever truncated — half a UTF-8
string is not a string — and nothing is ever stale, because the value is
produced fresh on the call that delivers it.

**(c) The host allocates in guest memory.** The guest exports `alloc`; the host
calls it and returns a pointer. *Failure mode: it breaks the rule this crate is
built around.* A host function would call into the guest **mid-call**, which is
the reentrancy violation that "Reentrancy" below exists to make impossible; and
it puts the ownership of every returned buffer on the guest, so a guest that
forgets to `dealloc` leaks and a guest that deallocs twice corrupts its own
heap. The guest bindings export `alloc` and `dealloc` and the host still calls
neither, and that is now a decision rather than a gap.

### The protocol, exactly

`get_attribute` and `text_content` **always return the value's full byte
length**, whether or not it fit. This is `snprintf`'s convention:

- `len <= cap` — the bytes are at `out_ptr`, and `len` of them are valid.
- `len > cap` — **nothing was written.** The guest resizes to `len` and calls
  again. The second call is sized from the host's own answer, so it cannot come
  back short.
- `cap == 0` — legal, and is how a guest asks for a length with nowhere to put
  the value. It costs the host-side allocation and delivers nothing, so it is a
  worse way to size a buffer than guessing and retrying.

The guest's buffer is bounds-checked **before the document is read**, so a
`(ptr, cap)` outside guest memory costs only the check. That ordering is not
cosmetic: the expensive half of a read is the `String` the facade allocates, and
failing after that point would allocate it and throw it away.

`has_attribute` needs none of this. It answers `0` or `1` and moves no payload
at all, which makes it the one read the atom design does help.

### `ABSENT`, the one negative that is not a failure

`getAttribute` returns `null` for an absent attribute, and `null` is not the
same as present-and-empty: a guest that cannot tell them apart cannot implement
`hasAttribute` on top of a read. So `get_attribute` needs three outcomes from
one `i32`, and `ABSENT` (`-9`) is the third.

It is the single exception to "negative is an error", and the alternatives were
both worse. Calling `has_attribute` first would double the crossings of the very
operation being measured, for a reason that has nothing to do with strings, and
would corrupt the measurement this crate exists to take. Returning `len + 1` and
reserving `0` for absent would put arithmetic in the host and in every guest
binding forever, to avoid spending one status code. A guest binding maps `ABSENT`
to `Ok(None)` and every other negative to an error, so no guest above the
bindings sees it as a failure — and the host does not record it in `last_error`,
because a guest polling for an optional attribute would otherwise leave the error
slot permanently set to something that never went wrong.

### The measurement: reading

From `tests/end_to_end.rs`, reading back the page the counter mounted. The
"before" column is the first measurement, taken against the facade's owning
readers; see "The second experiment" for what moved it.

| Operation | Calls | Bytes host → guest | Host `String` bytes, before | after |
| --- | --- | --- | --- | --- |
| `get_attribute` (`class`, `"count"`) | 1 | 5 | 5 | **0** |
| `get_attribute` (absent) | 2 | 0 | 0 | 0 |
| `get_attribute` (present, empty) | 1 | 0 | 0 | 0 |
| `text_content` (`"+10"`) | 1 | 3 | 3 | **0** |
| `has_attribute` | 2 | 0 | 0 (a lie, then) | 0 (true, now) |
| **total** | **7** | **8** | **8** | **0** |

**Compare the surviving column with the write direction.**

- Setting `class="count"` costs **0 bytes.** A handle and two atoms.
- Reading `class` back costs **5 bytes**, and now nothing else.

That is still the reversal, and it is still not marginal: an operation that was
free becomes one that pays its value in full. The atom is still free — the
*name* costs nothing, on a read as on a write — and it still buys nothing,
because the answer was never a name. What the second experiment removed was the
*surcharge*, not the payload.

**The comparison is stated at steady state, and that is the harshest version of
it, not the kindest.** The first write of `class="count"` was not free: it cost
10 bytes of interning, once, exactly as the mounting table records. What it
bought was that every write after it is free, forever. A read buys nothing of
the kind. The tenth read of the same unchanged attribute costs the same 5 bytes
as the first, because there is no place in the design for a returned value to be
amortised into. So the gap between the two directions does not narrow as a page
runs — it widens.

There is a third copy the counters cannot see, and it belongs in this table's
footnotes rather than out of the document: a guest binding that hands back an
owned `String` allocates the value again on the guest side. The ergonomic
`Element::get_attribute` therefore copies a 5-byte value twice, once into the
guest's buffer and once into its `String`. The `_into` variants, which write
into a `Vec<u8>` the guest reuses across frames, cost one. That one is the
payload and there is no removing it.

`has_attribute`'s zero used to be **a lie of omission**: `element::has_attribute`
went through the same `read_attr` as `get_attribute`, so it cloned the
attribute's value into a `String` and discarded it to answer a boolean. It no
longer does, so the zero is now the whole truth. The sentence that used to
qualify it is kept here because a reader comparing two runs of this table
deserves to know which zero they are looking at.

### The failure mode, measured

A 200-byte attribute, against the guest bindings' 64-byte first guess:

| | before | after |
| --- | --- | --- |
| Host calls for one read | 2 | **2** |
| Bytes host → guest | 200 | 200 |
| Host-side `String` bytes | 400 | **0** |
| Host allocations | 2 | **0** |

The second crossing is inherent to mechanism (b) and was never going to move: a
guest cannot size a buffer for a length it has not been told. The 400 bytes were
not inherent, and they are gone — neither call builds anything now, because the
first finds the value already contiguous in the document, measures it, and
declines to copy.

## The second experiment: was the read cost the boundary, or the facade?

The section above named a write-into-buffer reader as the obvious fix and
declined to build it, on the grounds that it was a second experiment needing a
baseline. This is that experiment, run against that baseline.

**The question.** A read of `n` bytes cost `n` across the boundary and `n`
allocated host-side. Was the second `n` a property of the boundary — something
any ABI over this document would pay — or an artifact of `blitz-dom-api`
returning owned `String`s?

**What was built.** Buffer-writing readers *beside* the owning ones, not
replacing them: `element::get_attribute_into` and `node::text_content_into`,
each taking a `&mut [u8]` and returning the full byte length under the same
`snprintf` contract this ABI already uses. `blitz-script` will still want the
owning readers, and they are untouched. `blitz-wasm`'s three readers now hand
the facade the guest's own buffer, so there is nothing in between.

**The result: `host_string_bytes` for reads went to zero, and `bytes_written`
did not move.** The cost was the facade. Both tables above carry the before and
after.

### The two things that made it possible

**`Memory::data_and_store_mut`.** The host has to hold guest memory and the
document at the same time to write one from the other. `read_string` deliberately
does the opposite — it drops its borrow of guest memory *before* touching the
document — and this crate's docs described that as the reentrancy rule.
It is not. `host_view` holds both at once and is **more** strongly safe: calling
into the guest requires the store, and it holds the store mutably, so a guest
call from in there does not typecheck rather than merely not happening. The rule
is "no guest call with a document borrow live", and holding two borrows is not
what breaks it.

**A private borrow inside the facade.** `element::find_attr` returns
`Option<&str>` into the document. The crate's borrow discipline — readers return
owned values, so no borrow survives a call — is a rule about its *public*
surface, and `find_attr` is `pub(crate)`. Every public caller either clones it
or writes it into a buffer the caller supplied.

### Where it did not work the same way, and why

The two readers are not the same shape, and the experiment separated them.

**`get_attribute` was a pure win.** An attribute's value exists in the document
as contiguous bytes. Reading it into a buffer is one `memcpy` and the owning
reader's `String` was overhead, start to finish.

**`text_content` traded an allocation for a traversal.** `textContent` is a
concatenation over a subtree; it does not exist anywhere until something builds
it, so no reader can hand back bytes that were already sitting there. The
allocation is gone — the bytes land in the caller's buffer — but honouring
"nothing is written unless it all fits" needs the length before the first byte
is written, and that costs a second walk. A single streaming pass would have
written `one ` before discovering that `two` did not fit, and the guarantee this
ABI states would have become false.

Whether one allocation is worth one extra pointer-chasing walk is a timing
question, and this crate measures bytes rather than time on purpose. What is not
a timing question: the allocation is gone, and a caller that already knows the
length can skip the measuring pass with `node::text_content_len`.

Both go through `blitz-dom`'s `write_text_content`, which was made public and
generic over `fmt::Write` so that the counting sink, the filling sink and
`String` all share one traversal. A private copy of that walk in the facade
would have disagreed with `blitz-dom`'s the first time a `NodeData` variant was
added.

### What it did not fix, and what that implies

`has_attribute` picked up a separate improvement on the way past: it went
through the same cloning helper, so it allocated the whole attribute value to
answer a boolean. It no longer does. That was never a boundary cost at all —
zero bytes crossed for it before and after — which makes it the cleanest example
of the category this experiment was looking for.

**The write direction still pays its `String`, and now that is the conspicuous
one.** `set_text` of `n` bytes still reports `host_string_bytes == n`, because
`read_string` copies guest memory into a `String` before the document is
touched. The counters module used to call that copy "not negotiable, the
reentrancy rule". **That was wrong, and `host_view` is the proof**: the same
technique that removed the read direction's copy would remove the write
direction's. It is not done here because this experiment was scoped to the read
side and because it would move numbers the mounting table asserts.

So the honest summary is narrower than "the facade was the cause". The facade
was the cause *of the read direction's surcharge*. The write direction's
surcharge is this crate's own, it is still there, and
`the_write_direction_still_pays_for_its_string` is what will notice when it
stops being.

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

| Operation | Tier | Direction | Bytes per call |
| --- | --- | --- | --- |
| `intern` | (b), by definition | guest → host | `len` |
| `create_element` | (a) | none | 0 |
| `create_text` | (b) | guest → host | `len` |
| `append_child` | neither: two handles | none | 0 |
| `set_attribute` | (a) for both name and value | none | 0 |
| `set_text` | (b) | guest → host | `len` |
| `add_listener` | (a) | none | 0 |
| `remove_listener` | neither: one listener id | none | 0 |
| `get_attribute` | (a) for the name, **neither for the value** | host → guest | `len` |
| `text_content` | (b), and it cannot be otherwise | host → guest | `len` |
| `has_attribute` | (a) | none | 0 |
| `dispatch` (export) | neither: one listener id | none | 0 |

The third tier the readers reveal is **(c): not a tier at all.** A returned
value is neither interned nor copied-by-agreement; it is copied because it is
the answer. Interning it would be absurd — an atom is never released, so
interning what a page *reads* would grow the host's table with the page's own
content — and there is nothing cheaper to send instead. That is the structural
reason reads reverse the result, stated without a measurement: the two tiers
that make writes cheap have no read-direction counterpart.

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
framework imports the same names as a guest carrying none.** The framework is
entirely on the guest side of the boundary, which is what
`the_guest_imports_only_the_blitz_module` asserts.

## Errors

**Every host function returns `i32`. Negative is an error, non-negative is
success**, with exactly one exception, `ABSENT`. For an operation that creates
something the value *is* the handle or atom; for a reader it is the byte length
of the value; for the rest it is `OK`, which is zero. Handles and atoms are
therefore capped at `i32::MAX`, which buys a single return value instead of an
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
| `-9` | `ABSENT` — **not an error.** See "The read direction". |

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

Five numbers per operation, and deliberately no timing. A timing number
measured on one machine, in one build profile, against an interpreter is not
evidence; a byte count is identical everywhere and is exactly what the boundary
design changes.

**The two directions are separate fields and there is no total that mixes
them.** A read byte and a write byte are not the same thing — they are produced
by different mechanisms and cost different amounts — so a number that added them
would be a number nobody could act on.

| Field | Direction | Counts |
| --- | --- | --- |
| `bytes_copied` | guest → host | bytes read **out of** guest linear memory |
| `bytes_written` | host → guest | bytes written **into** guest linear memory |
| `host_string_bytes` | neither | owned host-side `String` bytes the call had to materialise |
| `host_allocs` | neither | allocations this crate made or took ownership of |
| `calls` | | invocations, errors included |

`Counters::total_bytes_copied` and `Counters::total_bytes_written` are the two
per-direction totals; `total_bytes_crossed` is the grand total and is for a
grand total only, never for a claim about one direction.
`Op::payload_direction` says which way a given operation can move bytes at all,
so a report can group by direction without its author remembering the table.

`host_string_bytes` is **the copy that never crosses the boundary**, and it is
the number that stops a byte-across-the-boundary count from flattering itself.
It is also the number that paid for itself: both directions used to report it,
and finding out that only one of them had to is what "The second experiment"
above is.

- **Writing pays it.** `read_string` copies guest memory into a `String` before
  the document is touched, and the facade copies that `String` into the node. A
  `set_text` of `n` bytes reports `bytes_copied == n` and
  `host_string_bytes == n`.
- **Reading does not.** The facade's buffer-writing readers put the value
  straight into the guest's buffer. A read of `n` bytes reports
  `bytes_written == n` and `host_string_bytes == 0`.

**The asymmetry is this crate's, not the boundary's.** The write direction's
copy is not required by the reentrancy rule — `host_view` holds guest memory and
the document at once and is more strongly safe than dropping one first — it is
required by the shape `read_string` has. Removing it is the next experiment. Do
not read the write direction's `n` as inherent just because it is still there.

`dispatch` is counted here too, and it is the one counter that goes the other
way: a call the *host* made into the guest. It is in this table anyway because
it is the number that answers "what does a click cost at the boundary", which
is the question the whole event path exists to answer. It is excluded from
`total_calls`, which counts inbound calls and would answer no question at all
with an outbound one added to it.

`host_allocs` counts allocations **this crate** makes or takes ownership of: the
`String` built from guest memory on a write. A read takes ownership of nothing
now, so its count is zero. It does not count allocations the facade makes and
keeps, which this crate cannot see without instrumenting packages it does not
own, and it does not count the amortised growth of the listener table on
`add_listener`, which is not a per-call allocation. `add_listener`'s zero
therefore means "no string was built", not "no memory moved".

One omitted allocation is still there and is not negligible:

- `blitz_dom_api::document::create_element` lowercases the tag into a fresh
  `String`.

Two more were, and the second experiment removed them. `element::get_attribute`
still lowercases the queried name into a `String`, but the reader this crate
calls, `get_attribute_into`, compares byte by byte instead; and
`element::has_attribute` no longer clones the attribute's value to answer a
boolean. Both were costs with **zero boundary traffic** to justify them, which
is why a counter measuring only the boundary could never have found them.

A reader of these counters must still not take "zero host allocs" to mean
"nothing was allocated". It means this crate allocated nothing.

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
3. **The layout flush before a geometry read.** None of the eleven operations
   reads geometry — the three readers added for the read direction read
   attributes and text, which layout does not touch — so this does not bite
   yet. It will the moment
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
- **`alloc`/`dealloc` are exported by the guest and the host still never calls
  them** — and that is now a decision, not a gap. The read direction exists, and
  the mechanism chosen for it hands the host a buffer the guest already owns.
  Calling `alloc` would be mechanism (c): a host function calling into the guest
  mid-call, which is the one thing this ABI is built not to do. They stay
  exported because they cost nothing and an embedder placing bytes in the module
  from outside may want them.
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

## Templates and lists: what is settled, and what is blocked

`instantiate`, `set_binding` and `drop_instance` are **not bound**, because
`blitz-templates` does not exist — not in this workspace, not on any branch of
this repository. Nothing here can be built against a package that is not there,
and a binding written against a guessed template representation would be the
expensive kind of wrong.

What *is* settled is the list-key representation, because that could be settled
against real code rather than against a design. `map_array` has landed in
`SolidRS` (`src/map.rs`; the suite is 183 tests and passes). Reading it changed
the answer.

### What `map.rs` actually produces

- **A key is a type, not a value.** `K: Eq + Hash + 'static`, chosen by the
  guest. `map_array` (identity mode) sets `K = Item` and clones the item;
  `map_array_keyed` takes a key function; `map_array_by_index` has **no key at
  all** — its `same_position` predicate is the constant `true`, because rows are
  positional there.
- **A key never leaves the guest, and is never stored.** It is recomputed from
  the item on every pass, dropped into a `HashMap<K, isize>` for the duration of
  that pass, and discarded. `MapData` holds items, mappings, owners and signals;
  it does not hold keys.
- **Duplicate keys are legal.** `new_indices_next` chains them, scanning
  backwards so that duplicates match in natural order. Upstream permits this and
  the port preserves it.
- **Mapped-array identity is load-bearing.** A pass with no structural change
  returns the *same* `Rc<Vec<M>>`, and `Rc::ptr_eq` is the memo's comparator, so
  downstream consumers do not re-run at all.

### The representation that follows

**The key that crosses is a `u32` row id the guest issues, one per live row
scope. It is not the key, not a hash of the key, and not an atom.**

Each of the three alternatives fails against something in the list above:

- **The key's bytes.** `K` need not be a string, so there is often nothing to
  send; and when there is, this is Part 1's problem again — the whole list's
  keys would cross on every pass, and "The read direction" measured what
  per-frame string traffic costs.
- **A hash of the key.** `map.rs` resolves collisions with full `Eq` inside its
  `HashMap<K, _>`. The guest therefore has fidelity the host would not, and a
  colliding pair would fuse two distinct rows into one — the host reusing the
  wrong node for the wrong scope, silently, and only under collision.
- **An atom.** Atoms are never released, and that is right for names because
  names come from a small fixed vocabulary. Row keys come from the *data*. A
  list that scrolls a million rows past would add a million entries the
  interner can never free. A row id needs a free list; an atom is exactly the
  thing that does not have one.

A row id also resolves the duplicate-key problem rather than inheriting it. Two
rows with the same `K` are two row scopes, so they are two ids — the guest has
already disambiguated them with full `Eq` before anything crosses.

### The consequence for "two reconciliations over one identity"

This is the part worth stating plainly: **two independent reconciliations agree
only if they run the same algorithm.** If the host reconciled by `K` and the
guest reconciled by `K`, they would have to match on duplicate handling, on
prefix and suffix skipping, and on the order removals are committed in — and
where they differed, the guest's scope for one row would end up bound to the
host's node for another, silently and only on some edits.

Reconciling on the row id removes that requirement instead of documenting it.
Exactly one row exists per id, so the host's reconciliation is a lookup with no
ambiguity left in it, and the algorithm that resolved the ambiguity is the one
in `map.rs` — which is the one with the tests.

Two things follow for the calls, when they can be written:

1. `drop_instance(node)` corresponds to a row scope's disposal, which `map.rs`
   defers to commit time so that removals happen *after* the pass's new rows are
   created. The host must tolerate that order: a row id can be issued for a new
   row before the id of the row it displaced has been dropped.
2. `set_binding(node, binding_id, value)` needs the two tiers Part 1 measured,
   not one. A binding's *name* is a small fixed vocabulary and should be an atom
   like `binding_id` already is; a binding's *value* is per-frame content and
   must be copied `(ptr, len)`, for the same reason `set_text` is copied and
   `set_attribute` is not.

None of that is bound. It is written down so that when `blitz-templates`
arrives, the key question is already answered against code that exists.

## Not what the brief assumed

`blitz-script` does not yet bind `blitz-dom-api`; reparenting it is a separate,
unstarted change. So this crate is not the facade's second consumer, it is its
**first**. Anything awkward recorded above is therefore evidence about the
facade rather than about this binding, and the two entries under "Known costs"
in the facade's MAPPING.md were written from this crate's needs.

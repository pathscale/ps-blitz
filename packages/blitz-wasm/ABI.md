# The `blitz-wasm` ABI

Every decision the boundary makes, and the reasoning behind each. Numbers in
this document are asserted by `tests/end_to_end.rs`, not quoted from a run: if
one of them changes, that test fails.

## The imports

Six functions, all in the module named `blitz`.

| Import | Signature | Returns |
| --- | --- | --- |
| `intern` | `(ptr: i32, len: i32) -> i32` | atom, or error |
| `create_element` | `(tag_atom: i32) -> i32` | handle, or error |
| `create_text` | `(ptr: i32, len: i32) -> i32` | handle, or error |
| `append_child` | `(parent: i32, child: i32) -> i32` | `OK`, or error |
| `set_attribute` | `(node: i32, name_atom: i32, value_atom: i32) -> i32` | `OK`, or error |
| `set_text` | `(node: i32, ptr: i32, len: i32) -> i32` | `OK`, or error |

Five of these are the operation set the brief asked for. **`intern` is the
sixth and it is not optional.** Every one of the five takes names as atoms and
nothing else in the ABI produces an atom, so without it the interned tier is
unreachable and the five ops cannot be called at all. It is listed separately
throughout this document because it is also the only place a name is ever
copied, and folding its cost into the operations that consume atoms is exactly
how this measurement would end up overstating itself.

`set_text` maps onto `blitz_dom_api::node::set_text_content`, which rewrites a
text node in place and replaces an element's children with a single text node.
One import covers both the update case and the "empty this and put a string in
it" case, which is why there is no separate `set_data`.

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

### The measurement

From `tests/end_to_end.rs`, building this page:

```html
<div class="panel" id="root">
  <h1>Blitz</h1>
  <p class="row">one</p>
  <p class="row">two</p>
  <p class="row">three</p>
</div>
```

| Operation | Calls | Bytes copied |
| --- | --- | --- |
| `intern` | 8 | 25 |
| `create_element` | 5 | 0 |
| `create_text` | 4 | 16 |
| `append_child` | 5 | 0 |
| `set_attribute` | 5 | 0 |
| `set_text` | 0 | 0 |
| **total** | **27** | **41** |
| **total excluding interning** | | **16** |

The 25 bytes are the eight distinct names: `div`, `class`, `panel`, `id`,
`root`, `h1`, `p`, `row`. Every one crosses exactly once, no matter how many
elements later use it. The 16 are the four pieces of text, which are the only
thing on this page that is genuinely new information.

**Read the two totals together.** 16 bytes is what a mutation costs once the
vocabulary is known, which is the steady state a running page is in. 41 is what
the first paint costs including learning the vocabulary. Quoting only the first
would be a true number telling a false story, which is why
`Counters::total_bytes_copied` and
`Counters::bytes_copied_excluding_interning` both exist and the test asserts
both.

The demo module is 20,806 bytes built `--release`. Module size is not what this
crate optimises: the guest bindings use `std` rather than `no_std` because a
`no_std` wasm32 guest has no global allocator and no panic handler, and
supplying both by hand would shrink the module without moving a single
boundary byte.

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
having. When event dispatch arrives and has to call a guest export, the borrow
will already be gone for the same reason.

## Counters

Three numbers per operation, and deliberately no timing. A timing number
measured on one machine, in one build profile, against an interpreter is not
evidence; a byte count is identical everywhere and is exactly what the boundary
design changes.

`bytes_copied` counts bytes read out of guest linear memory, and nothing else.

`host_allocs` counts allocations **this crate** makes: the `String` built from
guest memory. It does not count allocations inside `blitz-dom-api` or
`blitz-dom`, which this crate cannot see without instrumenting packages it does
not own. Those exist and are not negligible:
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
   An embedder that has one requests the frame itself, gated on `mutated()`.
3. **The layout flush before a geometry read.** None of the six operations
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
  them.** With these six operations every string travels guest to host, so the
  guest allocates its own and the host only reads. They exist now because the
  first operation that returns a string to the guest needs them to already
  have a settled signature.
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

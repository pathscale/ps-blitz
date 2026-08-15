# blitz-wasm

A wasmi host binding over [`blitz-dom-api`](../blitz-dom-api), so a
WebAssembly guest can build a DOM, mutate it, and respond to events on it with
**no JavaScript anywhere in the path**.

This is the sibling of `blitz-script`: both bind the same facade, and neither
depends on the other. Nothing here imports `blitz-script`, and `blitz-dom-api`
must never learn that a wasm runtime exists.

Read [ABI.md](ABI.md) before using it. Every boundary decision is there, with
the reasoning, and the numbers in it are asserted by the test suite rather than
quoted from a run.

## Layout

```
src/            the host: linker registration, handle table, listeners, counters
guest/          a SEPARATE workspace, wasm32-unknown-unknown only
  bindings/     blitz-wasm-guest: safe Rust over the raw imports
  demo/         a reactive counter, built on solid_rs
tests/          builds the demo, runs it under wasmi, clicks it, asserts the DOM
```

## Using it

```rust
let mut store = Store::new(&engine, Host::new(document, mount_node_id));
let mut linker = Linker::<Host>::new(&engine);
blitz_wasm::add_to_linker(&mut linker)?;
let instance = linker.instantiate_and_start(&mut store, &module)?;
instance.get_typed_func::<(), i32>(&store, "run")?.call(&mut store, ())?;

if store.data().mutated() {
    store.data_mut().document_mut().resolve(0.0);
    store.data_mut().clear_mutated();
}
```

Delivering an event, which runs the guest's listeners once the document is
free again:

```rust
let dispatched = blitz_wasm::dispatch_dom_event(&mut store, &instance, event)?;
if store.data().redraw_requested() {
    store.data_mut().document_mut().resolve(0.0);
    store.data_mut().clear_redraw_request();
}
```

The guest side, with no `unsafe` and no status codes:

```rust
let panel = Element::new("div")?;
panel.set_attribute("class", "panel")?;
panel.append(text("hello")?)?;
Node::mount().append(panel.node())?;
panel.on("click", || { /* ... */ })?;

assert_eq!(panel.get_attribute("class")?.as_deref(), Some("panel"));
assert_eq!(panel.text_content()?, "hello");
```

## The eleven operations

`intern`, `create_element`, `create_text`, `append_child`, `set_attribute`,
`set_text`, `add_listener`, `remove_listener`, `get_attribute`, `text_content`,
`has_attribute`. Five are enough to build a page; `intern` is what makes those
five callable, since they all take names as atoms; the next two are what make
the page respond; the last three read it back. There is one export, `dispatch`.
The full 35 operations `blitz-dom-api` exposes are deliberately not ported yet.

## Reads go the other way, and cost more

The first eight operations all travel guest to host. The three readers reverse
that, and the mechanism is **the guest supplies the buffer**: `(ptr, cap)` in,
the value's full byte length out, and the guest retries with a bigger buffer if
it did not fit. The two alternatives — ask-then-fetch, and the host calling the
guest's `alloc` — are recorded in ABI.md with what each would have cost.

Reads are the axis this design does not win, and ABI.md says so with numbers
rather than hedging. Setting `class="count"` costs **0 bytes**; reading it back
costs **5**, because the atom design has nothing to amortise when the answer
*is* the payload. A repeated write stays free forever; a repeated read pays full
price every time, so the gap widens as a page runs.

It used to cost 5 more, allocated host-side, and that half is gone. The second
experiment — buffer-writing readers in `blitz-dom-api`, so the intermediate
`String` never exists — took `host_string_bytes` for reads from 5 to **0** with
the bytes across the boundary unchanged, and took the 200-byte overflow case
from 400 host-side bytes to **0**. The surcharge was the facade; the payload is
the boundary. ABI.md, "The second experiment", has the before and after for both
and the one place it did *not* work the same way.

**The write direction still pays its `String`**, and that is now this crate's
own doing rather than a rule it is obeying. See the same section.

## Events are dispatched *after* propagation

`EventHandler::handle_event` runs while the `EventDriver` holds the document,
so the guest is not called from there. The handler queues listener ids; the
host drains the queue afterwards, with the document owned again, and calls the
guest's `dispatch` export once per listener.

That keeps this crate's reentrancy rule intact — no document borrow is ever
held across a call into the guest — and it costs one thing: **a guest handler
cannot `preventDefault` or `stopPropagation`,** because by the time it runs the
default action has already happened. ABI.md states the deviation, the reasoning
and the shape of the eventual fix.

## What it costs

Mounting the counter: **23 host calls, 47 bytes copied across the boundary, 3
of them once the vocabulary is known.** Names cross once each and are `u32`
atoms thereafter, so `set_attribute` and `add_listener` copy nothing and
allocate nothing on the host side.

Clicking it: **zero bytes.** An event is a listener id; the only byte that
moves is the digit the effect writes back into the DOM.

Reading it back: **8 bytes host-to-guest, and nothing else** — it was 8 more
allocated host-side until the buffer readers landed. See "Reads go the other
way" above; the two directions are separate counters and there is no total that
mixes them.

`tests/end_to_end.rs` asserts those numbers, including the interning cost and
the host-side copies. A "zero bytes copied" claim that omits what interning cost
would be a true number telling a false story, and so would a read measured only
at the boundary.

## Tests

```
cargo test -p blitz-wasm
```

Builds the demo guest to `wasm32-unknown-unknown`, instantiates it under wasmi,
runs it against a real `blitz-dom` document, synthesises clicks on it, and
asserts the resulting tree, its layout, its text after each click, and the
counters. Requires the `wasm32-unknown-unknown` target; the test will not
install it for you, and says so if it is missing. The first build also needs
network access, to fetch the guest's pinned `solid_rs` dependency.

The event tests all end at the DOM. A test that asserted only "dispatch
returned OK" would pass against a guest whose handler does nothing.

Compiling to wasm32 only proves the code type-checks. Instantiating proves it
links, which is where a missing panic handler or an unsatisfied import actually
shows up, so the test builds a real `.wasm` rather than calling the guest crate
as a library.

## The demo

`guest/demo` is a counter built on [`solid_rs`](https://github.com/pathscale/SolidRS):
a signal holding the count, a click listener that increments it, and an effect
that writes it into a text node. One click runs

```
click -> host queue -> guest dispatch -> signal write
      -> microtask drain -> effect -> set_text -> redraw
```

with no JavaScript in any step. The effect writes into a `<span>` the handler
never mentions, so a guest that simply set text on its own event target would
fail the test — the reactive graph in the middle is load-bearing.

It also exports `echo`, which reads the tree back out of the host and writes
what it read into the document verbatim, and `probe_forged`, which calls the
readers with a handle the host never issued and a buffer outside its own memory
and checks that it was told so rather than killed. Those two are how the read
direction is tested at the DOM rather than at a return code.

A guest carrying a whole reactive framework imports the same names as a guest
carrying none, which is the sharpest statement of where the boundary is.

## Templates and lists

`instantiate` / `set_binding` / `drop_instance` are not bound: `blitz-templates`
does not exist yet. The list-key question *is* settled, against `map_array` in
[`SolidRS`](https://github.com/pathscale/SolidRS) rather than against a design —
the key that crosses is a `u32` row id the guest issues per live row scope, not
the key, not a hash of it, and not an atom. ABI.md has the three reasons and
what each alternative breaks.

## Not here

The other 24 facade operations, `preventDefault`, event objects with payloads,
a write-into-buffer reader, and anything from the `chuzz` repo. A click carries
its listener id and nothing else.

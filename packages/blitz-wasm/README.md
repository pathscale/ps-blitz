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
```

## The eight operations

`intern`, `create_element`, `create_text`, `append_child`, `set_attribute`,
`set_text`, `add_listener`, `remove_listener`. Five are enough to build a page;
`intern` is what makes those five callable, since they all take names as atoms;
the last two are what make the page respond. There is one export, `dispatch`.
The full 35 operations `blitz-dom-api` exposes are deliberately not ported yet.

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

`tests/end_to_end.rs` asserts those numbers, including the interning cost. A
"zero bytes copied" claim that omits what interning cost would be a true number
telling a false story.

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

A guest carrying a whole reactive framework imports the same eight names as a
guest carrying none, which is the sharpest statement of where the boundary is.

## Not here

The other 27 facade operations, `preventDefault`, event objects with payloads,
and anything from the `chuzz` repo. A click carries its listener id and nothing
else.

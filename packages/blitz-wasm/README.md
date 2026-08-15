# blitz-wasm

A wasmi host binding over [`blitz-dom-api`](../blitz-dom-api), so a
WebAssembly guest can build and mutate a DOM with **no JavaScript anywhere in
the path**.

This is the sibling of `blitz-script`: both bind the same facade, and neither
depends on the other. Nothing here imports `blitz-script`, and `blitz-dom-api`
must never learn that a wasm runtime exists.

Read [ABI.md](ABI.md) before using it. Every boundary decision is there, with
the reasoning, and the numbers in it are asserted by the test suite rather than
quoted from a run.

## Layout

```
src/            the host: linker registration, handle table, counters
guest/          a SEPARATE workspace, wasm32-unknown-unknown only
  bindings/     blitz-wasm-guest: safe Rust over the raw imports
  demo/         a demo guest that builds a page
tests/          builds the demo, runs it under wasmi, asserts the tree
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

The guest side, with no `unsafe` and no status codes:

```rust
let panel = Element::new("div")?;
panel.set_attribute("class", "panel")?;
panel.append(text("hello")?)?;
Node::mount().append(panel.node())?;
```

## The six operations

`intern`, `create_element`, `create_text`, `append_child`, `set_attribute`,
`set_text`. Five are enough to build a page; `intern` is what makes the other
five callable, since they all take names as atoms. The full 35 operations
`blitz-dom-api` exposes are deliberately not ported yet.

## What it costs

Building a five-element page with four pieces of text: **27 host calls, 41
bytes copied across the boundary, 16 of them once the vocabulary is known.**
Names cross once each and are `u32` atoms thereafter, so `set_attribute` copies
nothing and allocates nothing on the host side.

`tests/end_to_end.rs` asserts those numbers, including the interning cost. A
"zero bytes copied" claim that omits what interning cost would be a true number
telling a false story.

## Tests

```
cargo test -p blitz-wasm
```

Builds the demo guest to `wasm32-unknown-unknown`, instantiates it under wasmi,
runs it against a real `blitz-dom` document, and asserts the resulting tree,
its layout, and the counters. Requires the `wasm32-unknown-unknown` target; the
test will not install it for you, and says so if it is missing.

Compiling to wasm32 only proves the code type-checks. Instantiating proves it
links, which is where a missing panic handler or an unsatisfied import actually
shows up, so the test builds a real `.wasm` rather than calling the guest crate
as a library.

## Not here

Event dispatch, the other 30 facade operations, and anything from the `chuzz`
repo. Events need a dispatch model that calls back into the guest; the
reentrancy rule in ABI.md is written now so that the borrow discipline is
already correct when they land.

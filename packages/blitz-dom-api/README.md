# blitz-dom-api

Runtime-agnostic DOM operations over `blitz-dom`.

`blitz-script` is currently the only path from a language runtime to the DOM,
and its operations are entangled with Boa: `JsValue` in, `JsResult` out, a
`Context` threaded through, prototype objects holding the registration. A
second runtime cannot reuse any of it. This crate is those operations with the
runtime removed, so that a binding does argument coercion and result
construction and nothing else.

Nothing here depends on Boa, and nothing here may: `tests/no_boa.rs` resolves
the real dependency graph and fails if a `boa_*` crate appears in it.

`blitz-script` is untouched. Reparenting it onto this crate is a separate
change.

## Using it

```rust
use blitz_dom_api::{document, element, node, style};

let panel = document::create_element(&mut doc, "div")?;
element::class_list_add(&mut doc, panel, &["panel"])?;
style::set_property(&mut doc, panel, "width", "200px")?;
node::append_child(&mut doc, body, panel)?;
```

Document first, then the node the operation is on (what a binding calls
`this`), then the arguments in the order the DOM method declares them. Readers
take `&BaseDocument`, mutators `&mut BaseDocument`, everything returns
`Result<T, DomError>`.

## Two rules worth reading before you use it

**No return value keeps a document borrow alive.** Readers that would naturally
hand back `&str` return owned `String`. That is a deliberate allocation: a
binding calls out to guest code between operations and guest code calls back
in, and a borrow still live across that boundary panics inside `RefCell` at a
call site with no relationship to the code that took it.

A binding that does not want the `String` does not have to pay for it. The
buffer-writing readers — `element::get_attribute_into`, `node::text_content_into`
and `node::text_content_len` — write into a slice the caller owns and return the
value's full byte length, so nothing is allocated and nothing is truncated. They
sit beside the owning readers rather than replacing them. `blitz-wasm` measured
what the difference is worth; see MAPPING.md, "Readers allocate a `String`".

**Mutations do not mark layout dirty and do not request a redraw.**
`blitz-script` routes every mutation through `DomCtx::mutate_doc`, which does
both. Both are properties of the embedding, so both stay with the binding. A
binding that forgets the flag reads geometry from before its own mutations; one
that forgets the redraw makes a change that never reaches the screen.

The same applies to `geometry::bounding_client_rect`, which reads layout
without flushing it. The caller flushes, then reads. See MAPPING.md.

## What is not here

**Events.** Event objects, `addEventListener` / `removeEventListener` /
`dispatchEvent`, the propagation path, and the `on<event>` IDL properties.

**Selection.** `document.getSelection` and everything reachable from it.

**Pointer capture.** `setPointerCapture`, `releasePointerCapture`,
`hasPointerCapture`.

**Focus.** `element.focus`, `element.blur`, `document.activeElement`.

These are not omissions of convenience. Each needs a dispatch model — a
listener registry keyed by *a guest callable*, a propagation walk that invokes
guest code and observes what it did, a notion of object identity so that
removing a listener can find the one that was added. None of that can be
expressed over a borrowed document without inventing the runtime's object model
first, and inventing it here would mean the second runtime inherits the first
one's shape. They belong with the binding until there are two bindings to
generalise from.

Also absent, for the same reason: wrapper caching and node identity, argument
coercion, and the custom-element upgrade that must follow an insertion.
MAPPING.md lists these under "needs a runtime decision".

## Reading the code

- `document` — node creation and lookup
- `node` — tree structure, tree mutation, text content
- `element` — attributes, `classList`, `innerHTML`, selector matching
- `character_data` — `data`
- `style` — the inline `style` attribute
- `geometry` — `getBoundingClientRect`, the one operation that reads layout
- `atom` — `AtomId` and `Interner`, for a guest that cannot cheaply pass
  strings. Nothing requires it yet; see the module docs for why the operations
  still take `&str` and where the ownership boundary is.

MAPPING.md has a row per operation: upstream `file:line`, the facade function,
and whether the semantics are identical or deliberately differ.

## Tests

```
cargo test -p blitz-dom-api
```

A unit test per operation, asserting the copied edge cases rather than only the
happy paths; `tests/tree_and_layout.rs`, which builds a document through the
facade alone and then asserts exact laid-out geometry; and `tests/no_boa.rs`.

The one test that is *not* here is parity against `blitz-script`, because a
dev-dependency on `blitz-script` would put Boa in this crate's graph and fail
`no_boa`. MAPPING.md says where that test belongs instead.

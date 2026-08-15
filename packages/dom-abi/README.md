# `dom-abi`

The contracts between a WebAssembly guest and a DOM host. Types, enums and
constants — there is no parser here, no DOM, no engine and no emitter.

One dependency, `serde`, and `tests/only_serde.rs` is what keeps it that way.

## Why this is a separate crate

```text
dom-abi
   ├── solid-layouts-oxc          writes templates   (pulls in oxc, napi)
   ├── ui-templates               reads them         (pulls in the engine)
   ├── blitz-wasm                 host ABI constants
   └── blitz-wasm/guest/bindings  same constants, other side
```

Put these types in the compiler and a browser engine depends on a JavaScript
parser. Put them in the runtime and the compiler depends on the engine. Neither
is a dependency anybody would defend; both are what you get by default if the
shared vocabulary lives in either place. So it lives in neither. Same shape as
`blitz-dom-api`, third time.

It is `publish = false`. The name is not settled and the format will churn, and
publishing is the moment additive-evolution discipline starts applying to
strangers rather than to this repository.

## Three modules, three independent versions

| Module | Constant | Covers |
| --- | --- | --- |
| `template` | `TEMPLATE_FORMAT_VERSION` | what a compiler writes and a runtime reads |
| `host` | `HOST_ABI_VERSION` | the calling convention across the boundary |
| `runtime` | `RUNTIME_ABI_VERSION` | how a module declares what it links against |

**They are independent, and that is why there are three.** A calling convention
can change without invalidating a single cached template: the templates did not
change, so making them look as though they had would discard every cached
artifact to report a change that did not touch them.

Independence is not a claim, it is a property that has to be maintained. No
module may consult another module's constant, and `tests/versions.rs` reads the
source to assert that none does — outside doc comments, which are exempt
because each constant is documented as independent of the other two and that
means naming them.

## The four rules

Each rule lives in a doc comment beside the thing it constrains. They are
collected here as an index, not as their home.

### 1. No computation

A binding is an opaque slot. There is no expression type in `template` and there
must never be one — not a small one, not "just for concatenation".

The absence is the feature. A template that cannot compute is one whose entire
behaviour is "put this value in this place", which is what separates
presentation from behaviour **at runtime** rather than by convention. Add an
expression type and the separation becomes a code-review rule, which is to say
it stops existing. Every expressive gap this creates closes the same way: the
guest computes the value and writes it to a binding.

`AttrValue::Variant` is the one shape that looks like the rule being bent. It is
closed — three atoms and a boolean, no nesting, no growth — where an expression
type is open, and it exists so that a variant class list stays a compile-time
set of atoms instead of becoming per-frame string traffic. If a third
alternative is ever needed it is a new variant, not a fourth field.

### 2. Stable ids, not positions

Templates ship separately from their guests: cached, CDN-hosted, embedded in a
`pathscale.templates` section built at a different time from the guest that
reads it.

If a binding were "the third hole in document order", inserting a static
`<span>` above it — a change that cannot affect behaviour — would renumber it,
and a guest compiled against the old numbering would write the wrong value into
the wrong hole with no error anywhere. So `BindingId`s are assigned, stable
across recompilations that do not change the binding, and never reused. Position
in `Template::bindings` carries no meaning.

### 3. Additive evolution only

Add variants. Never reorder meaning, never reuse a removed variant's name.

- **New variants are additive.** An old reader rejects an unknown variant
  loudly, which is correct: it genuinely cannot render it.
- **New struct fields are additive** given `#[serde(default)]`, which is why
  every collection field has it.
- **Nothing sets `deny_unknown_fields`,** so an older reader ignores fields it
  does not know. The cost is stated up front rather than discovered later: **a
  new field is silently dropped by an old reader**, so no future field may be
  load-bearing for correctness or safety. A field that must be honoured is a new
  variant, or a version bump.

Reordering a variant would silently reinterpret every cached template. Reusing a
removed name would do it to exactly the templates written while the name meant
the other thing, which is worse because it is intermittent.

### 4. Nothing data-derived becomes an atom

**Atoms are never released.** No free list, no refcount, no eviction. That is
right for names — tag names, attribute names, event names, values from a set
fixed at compile time — because a page's vocabulary is small and stops growing
once it is known, so an interned name costs its bytes once and is free
thereafter however many elements use it.

Applied to anything the data produces, the same property is an unbounded leak. A
list that scrolls a million rows past would add a million entries the host can
never reclaim. `Atom` does not promise "this is a string"; it promises "this
string comes from a set whose size does not depend on the data". Nothing in the
type system checks that. Values that vary go through a binding and cross in the
copied tier.

## Row identity

`For` does not carry a key. The guest issues a `RowId(u32)` per live row scope
and that is what crosses.

This was settled by reading `SolidRS`'s `map.rs`, not designed against it. The
alternatives each fail against something that file does: a key is a type rather
than a value (`K: Eq + Hash`, often not a string, never stored, recomputed each
pass); a hash of the key loses the collision resolution `map.rs` performs with
full `Eq`, so a colliding pair would silently fuse two rows; and an atom is the
one thing with no free list, which is what row keys drawn from data need.

The reason matters more than the rule. Two independent reconciliations agree
only if they run the same algorithm down to duplicate handling and removal
order — and `map.rs` permits duplicate keys, chaining them through
`new_indices_next`. Reconciling on a row id removes that requirement instead of
documenting it.

One consequence for any host: **a new row's id may be issued before the
displaced row's id is dropped.** `map.rs` defers disposal to commit, so the
exiting rows are disposed after the pass's new rows exist. A host that asserted
an id was free before issuing it would fail on exactly the reorderings this
design exists to handle.

## Events

`StopPropagation` is a flag set at registration, not a decision taken during
dispatch.

Dispatch is deferred: the host records which listeners matched, finishes
propagating, releases the document, and only then calls the guest. So a handler
cannot cancel an event it has not been called for yet — by the time it runs,
propagation is over. A runtime `stopPropagation()` would be a method that
compiles, runs and does nothing.

Declaring the intent up front is what makes it expressible at all, and one real
component needs it: a tab close button inside a clickable tab, on the first
click rather than the second.

## Canonicalisation and the hash

### The decision

**The hash is over the parsed structure, not over the encoded bytes.**

The alternative — specify a canonical text form and hash that — was rejected for
the reason that makes the decision necessary in the first place. A text encoding
hashes differently for the same meaning depending on whitespace, on indentation,
on how the emitter renders an empty sequence, and on field order if the format
admits maps. Pinning that down means pinning down an emitter, which this crate
would have to either depend on or describe without being able to check. It would
also make the hash depend on the output being text, which nothing here may.

The traversal is specified in `template::canonical`: depth-first, declaration
order, `u32` little-endian, strings length-prefixed and byte-exact with no
normalisation, sequences count-prefixed and ordered, enums preceded by a
one-byte tag from that module. Both sides implement it independently; no shared
code and no shared encoder are involved.

Excluded from the hash: `Template::hash` itself, and the debug names
(`Template::name`, `Binding::debug`, `Component::debug`). Excluding the names is
what makes renaming free — a rename does not invalidate a cache and does not
break a `Component` reference. Two templates differing only in their names
collide, which is correct: they render identically.

A tag is assigned once and never changes. Reordering them would change the hash
of every template using the affected variant, and — worse than invalidating
caches — would make two compilers at different versions disagree about a
component's identity.

### The algorithm and its prefix

**BLAKE3, 256-bit output, lowercase hex, prefixed `b3:`.**

The prefix is not decoration and it is not for a human reader: it is what makes
changing the algorithm additive. A hash written by a future compiler reads as
some other prefix, and an old reader rejects it as an unknown algorithm rather
than comparing 64 hex digits of a different function against 64 of this one and
concluding the content differs.

`ContentHash` does not compute hashes and cannot — a hash function is a
dependency and this crate has one. It carries the value and validates its shape:
prefix, length, lowercase hex. Uppercase is rejected rather than normalised,
because two spellings of one hash compare unequal as strings and this type is
used as a key.

## RON is the encoding, not the format

RON is what these types are written in today. Nothing here may depend on the
output being text, so moving to a binary encoding is a serde attribute change in
the consumers rather than a second implementation of anything.

`ron` is a dev-dependency, because the round-trip test needs *an* encoding to
round-trip through. It must never reach the shipped graph: a consumer that wants
CBOR or postcard should get it by choosing a different serde format, not by
taking RON along. `only_serde.rs` asserts both halves.

## The limit: types specify shape, not coherence

`Show { when: BindingId }` does not guarantee that id exists in the binding
table. A `ContentHash` is checked for being a hash, not for naming a component
that exists. An `Atom` holding a per-row value is well-formed and is a memory
leak. A well-typed template can be nonsense.

So **both sides still validate**, and the runtime validates **independently**
rather than trusting that the compiler already did. A template can arrive from a
CDN. It did not necessarily come from your compiler — and even when it did, it
came from whichever version of your compiler was current when it was cached.

## Tests

| File | What it holds down |
| --- | --- |
| `tests/only_serde.rs` | one dependency, asserted against the resolved graph; `ron` confined to the dev graph |
| `tests/round_trip.rs` | every node variant survives encode and decode, and the fixture is still exhaustive |
| `tests/versions.rs` | the three version constants exist, and no module reads another's |

`only_serde.rs` matches package names whole, against the first token of a
`cargo tree` line. The equivalent test in `blitz-dom-api` looks for Boa crates
and failed on its first run because a substring search for "boa" matches
`keyboard-types`. The trap here has a sharper edge: a substring search for
"serde" accepts `serde_json` and every other crate in that family, so the naive
version of this test would wave through exactly what it exists to catch.

## Not yet adopted

`blitz-wasm/src/status.rs` still declares its own copy of the status codes, and
`blitz-wasm/ABI.md` is still where the prose lives. Replacing those with
`host::Status` is a follow-up in that crate, not a change this one can make on
its behalf. Until it happens the codes exist in two places, which is the
condition this module was written to end.

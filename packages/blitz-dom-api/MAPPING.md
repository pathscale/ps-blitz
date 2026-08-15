# Mapping from `blitz-script` to `blitz-dom-api`

Every operation in this crate, where it came from, and whether it behaves
identically. Line numbers are against `packages/blitz-script/src/dom/*.rs` at
commit `a7fa525e`.

**Count.** 46 operations were listed as in scope; 46 have rows below, plus one
addition (`style::set_css_text`, the setter half of the `cssText` accessor,
without which the accessor cannot be reparented). Nothing in scope was skipped.

"Identical" means: same inputs in the same order, same outputs, same error
cases, same edge-case behaviour, so that the upstream function body becomes
`coerce args` + one call to this + `build result`. Every row marked
**different** says why, and says what the reparenting diff has to add back.

## Document — `document.rs`

| Operation | Upstream | Facade | Semantics |
| --- | --- | --- | --- |
| `createElement` | `document.rs:103` | `document::create_element` | identical (tag ASCII-lowercased here too) |
| `createElementNS` | `document.rs:116` | `document::create_element_ns` | identical (namespace first, tag not lowercased) |
| `createTextNode` | `document.rs:211` | `document::create_text_node` | identical |
| `createComment` | `document.rs:272` | `document::create_comment` | identical |
| `getElementById` | `document.rs:285` | `document::get_element_by_id` | identical |
| `querySelector` | `document.rs:293` | `document::query_selector` | **different**: a selector that will not parse is `Err(InvalidSelector)`, not `null`. Reparenting adds `.ok().flatten()` |
| `querySelectorAll` | `document.rs:301` | `document::query_selector_all` | **different**: same, upstream's fallback is `.unwrap_or_default()` |
| `documentElement` | `document.rs:68` | `document::document_element` | identical |
| `body` | `document.rs:75` (helper `find_tag`, `document.rs:41`) | `document::body` | identical, including the three-stage search |
| `head` | `document.rs:82` | `document::head` | identical |
| `importNode` | `document.rs:136` | `document::import_node` | identical: still delegates to `clone_node`, for the same reason |

## Node — `node.rs`

| Operation | Upstream | Facade | Semantics |
| --- | --- | --- | --- |
| `appendChild` | `node.rs:300` | `node::append_child` | **different**: the custom-element upgrade at `node.rs:320` is not here. It constructs a guest object, so it stays in the binding, immediately after this call |
| `removeChild` | `node.rs:441` | `node::remove_child` | identical, including that the parent argument is ignored and that the node is detached rather than dropped |
| `replaceChild` | `node.rs:453` | `node::replace_child` | identical (arguments new-then-old, returns the old node) |
| `insertBefore` | `node.rs:408` | `node::insert_before` | identical. The null reference node is `Option::None` here, which is the coercion upstream does at `node.rs:415` |
| `firstChild` | `node.rs:162` | `node::first_child` | identical |
| `nextSibling` | `node.rs:200` (helper `node.rs:184`) | `node::next_sibling` | identical |
| `previousSibling` | `node.rs:193` | `node::previous_sibling` | identical |
| `parentNode` | `node.rs:132` | `node::parent_node` | identical. Upstream aliases `parentElement` to the same function, and so does this |
| `childNodes` | `node.rs:151` | `node::child_nodes` | identical in content; a `Vec` snapshot rather than a live list, which is what upstream materialises anyway |
| `cloneNode` | `node.rs:592` | `node::clone_node` | identical, including the empty-comment placeholder for node kinds it cannot reproduce |
| `contains` | `node.rs:493` | `node::contains` | identical (inclusive, so a node contains itself) |
| `hasChildNodes` | `node.rs:482` | `node::has_child_nodes` | identical |
| `textContent` | `node.rs:227` | `node::text_content` | identical; returns `String`, see the borrow discipline in the crate root |
| `textContent =` | `node.rs:238` | `node::set_text_content` | identical, including detaching rather than dropping the previous children |
| `compareDocumentPosition` | `node.rs:512` | `node::compare_document_position` | **different**: upstream's two `expect`s become `Err(TreeInvariant)`. A facade must not abort the process over document state. Same bit values otherwise |

## Element — `element.rs`

| Operation | Upstream | Facade | Semantics |
| --- | --- | --- | --- |
| `getAttribute` | `element.rs:306` (helper `read_attr`, `element.rs:194`) | `element::get_attribute` | identical, including the ASCII-lowercasing of the name and `None` for absent |
| `setAttribute` | `element.rs:317` (helper `write_attr`, `element.rs:205`) | `element::set_attribute` | **different**: does not mark layout dirty and does not request a redraw. Both belong to the embedding; see the crate root |
| `removeAttribute` | `element.rs:327` (helper `clear_attr`, `element.rs:216`) | `element::remove_attribute` | **different**: same reason |
| `hasAttribute` | `element.rs:336` | `element::has_attribute` | identical |
| `tagName` | `element.rs:243` | `element::tag_name` | identical (upper-cased, empty for a non-element) |
| `classList.add` | `element.rs:723` | `element::class_list_add` | identical, including validating every token before writing any |
| `classList.remove` | `element.rs:740` | `element::class_list_remove` | identical |
| `classList.toggle` | `element.rs:753` | `element::class_list_toggle` | identical; the optional `force` is `Option<bool>` |
| `classList.contains` | `element.rs:711` | `element::class_list_contains` | identical |
| `innerHTML` | `element.rs:870` | `element::inner_html` | identical |
| `innerHTML =` | `element.rs:885` | `element::set_inner_html` | identical. Worth stating rather than assuming: what gets parsed comes from the document's `html_parser_provider`, and the default one parses nothing, so against a `DocumentConfig::default()` document this only empties the element |
| `matches` | `element.rs:1211` | `element::matches` | **different**: unparseable selector is `Err(InvalidSelector)`. Upstream's `is_ok_and` maps it to `false` |
| `closest` | `element.rs:1223` | `element::closest` | **different**: same. Upstream's `unwrap_or_default` searches an empty match set |
| `getBoundingClientRect` | `element.rs:1127` | `geometry::bounding_client_rect` | **different**: does not flush layout. Flushing is an obligation on the caller, see below |

### The flush obligation on `getBoundingClientRect`

**This is an obligation on callers, not a property of the facade.** It is the
only item in this document that causes a silent behaviour change if missed:
nothing errors, nothing warns, the read simply answers with the layout from
before the caller's own mutations.

The state. `blitz-script` flushes at `dom/element.rs:1135`, calling
`DomCtx::flush_layout` before it reads. That resolves the document only if
script has mutated it since the last frame, tracked by a dirty flag that every
mutation through `DomCtx::mutate_doc` sets and the flush clears. The flag is
runtime state: an operation over a borrowed `&BaseDocument` has no access to
it, and a facade that resolved unconditionally would turn a cheap read into a
full layout pass on every call.

Therefore:

- **Any binding must flush before calling `geometry::bounding_client_rect`.**
  The facade will not do it and cannot detect that you did not.
- **Reparenting `blitz-script` must keep the `ctx.flush_layout()` call at
  `dom/element.rs:1135`.** Only the read below it is replaced. Deleting the
  flush on the assumption that the facade inherited it is precisely the silent
  regression this section exists to prevent: the composer's autosize measures,
  writes, and measures again, and it would measure the pre-write layout every
  time.

`geometry::bounding_client_rect`'s own documentation states this, and
`geometry::tests::the_read_does_not_flush_layout` asserts the facade really
does not flush, so the obligation cannot quietly stop being real.

## CharacterData — `node.rs`

Registered on the CharacterData prototype at `node.rs:87` as `data`, backed by
the same functions as `Node.nodeValue`.

| Operation | Upstream | Facade | Semantics |
| --- | --- | --- | --- |
| `data` | `node.rs:270` | `character_data::data` | identical. Both were fixed in the same change, see below |
| `data =` | `node.rs:281` | `character_data::set_data` | **different**: does not mark layout dirty. Otherwise identical, and both were fixed in the same change, see below |

### The comment character-data bug, fixed across three sites

Per the DOM specification `Comment` inherits `CharacterData`, so `comment.data`
returns and sets the comment's text and `Node.nodeValue` does the same. The
contents were already on the node, at `blitz-dom/src/node/node.rs:750`
(`NodeData::Comment { contents: String }`), and nothing read them.

The asymmetry was worse than a wrong getter. `clone_node` copied the contents
from the start, so a script could clone a comment and read back text the
original refused to report; and the setter was a silent no-op, so fixing only
the getter would have traded one asymmetry for a worse one, with reads
returning contents and writes vanishing.

Three sites, fixed together in one change so that reparenting still changes no
behaviour:

1. `blitz-dom/src/mutator.rs:183`, `set_node_text` fell through to `_ =>
   return` for anything that was not a text node. It now has a
   `NodeData::Comment { ref mut contents }` arm. Deliberately **not** the text
   arm's damage handling: a comment generates no layout box, so
   `insert_damage(ALL_DAMAGE)` and `mark_ancestors_dirty()` would schedule a
   relayout for a change that cannot affect a pixel, once per write. Asserted
   by `mutator::test::setting_a_comments_data_does_not_dirty_layout`, which
   carries a text-node write as a positive control so the assertion is capable
   of failing.
2. `blitz-script/src/dom/node.rs:276`, `node_value` returned `js_str("")` for a
   comment and now returns the contents. `set_node_value` at `:281` needed no
   change once site 1 landed, since it already routes through `set_node_text`.
3. `blitz-dom-api/src/character_data.rs`, `data` returned `Some(String::new())`
   for a comment and now returns the contents.

`character_data::tests::a_comments_data_round_trips_through_a_write_and_a_clone`
covers the whole path: read, write, read back, clone, read the clone. The clone
leg is the one that would have caught the original disagreement.

## CSSStyleDeclaration — `style.rs`

Every one of these reads or writes the inline `style` attribute only; none
consults computed style. That is upstream's limitation too, and it is why
`getPropertyValue` answers `""` for a property a stylesheet set.

| Operation | Upstream | Facade | Semantics |
| --- | --- | --- | --- |
| `getPropertyValue` | `style.rs:262` | `style::get_property_value` | identical, including the naive `;`/`:` splitting that mis-parses `url(a;b)` |
| `setProperty` | `style.rs:223` (helper `update_style_attr`, `style.rs:201`) | `style::set_property` | **different**: does not mark layout dirty. Value semantics identical, including that an empty value removes the declaration |
| `removeProperty` | `style.rs:241` | `style::remove_property` | **different**: same. Returns the removed value, `""` if absent, as upstream does |
| `cssText` | `style.rs:154` | `style::css_text` | identical |
| `cssText =` | `style.rs:165` | `style::set_css_text` | **different**: does not mark layout dirty. *Addition*: not in the listed scope, but `cssText` is an accessor and the getter alone cannot be reparented |

## Needs a runtime decision

Nothing in the listed scope was blocked. These are the things a binding must
supply around the facade, recorded here because a reparenting that omits any of
them compiles and then misbehaves:

1. **Layout dirtiness and redraw requests.** `DomCtx::mutate_doc` sets a flag
   and calls `shell_provider.request_redraw()`. Every mutating operation in
   this crate omits both. A binding that forgets the flag gets geometry reads
   from before its own mutations; one that forgets the redraw gets a mutation
   with no event behind it that never reaches the screen.
2. **Custom element upgrade.** `node.rs:320` runs `upgrade_if_defined` after
   an insertion, because insertion is when `connectedCallback` is due.
3. **The layout flush before a geometry read.** See above.
4. **Wrapper identity and caching.** `node_wrapper` (`mod.rs:65`) exists so a
   node is always the same guest object. That is a property of the guest's
   object model, not of the DOM.
5. **The camelCase-to-CSS property name mapping.** `style.maxHeight = "4px"`
   goes through `css_property_name` (`style.rs:17`) before reaching the
   declaration list, and `-webkit-transform` depends on its leading-dash rule.
   That transformation belongs to the *property-access* idiom, not to
   `setProperty`: upstream's `set_property` takes the name as given and so does
   `style::set_property` here. A binding that exposes property access has to
   apply it on the way in.
6. **Argument coercion.** Upstream's `to_rust_string` runs ECMAScript
   `ToString`, so `setAttribute("x", 1)` writes `"1"`, and a missing argument
   becomes the string `"undefined"` (except in the `Text` constructor, which
   special-cases it at `document.rs:261`). A different runtime will coerce
   differently; that is the binding's business and deliberately not this
   crate's.

## Known costs, and what they are not evidence about

Two deliberate choices here show up in a `blitz-wasm` profile. Both are recorded
so that a future reader measuring that profile attributes them correctly, rather
than reading them as an argument against the design they sit next to.

One of the two has since been measured and addressed, and its entry says so
rather than being quietly rewritten. A prediction that turned out right is worth
more on the record than off it.

### Readers allocate a `String` — and the buffer variants that do not

**Status: measured, then fixed.** This entry is kept in full rather than
deleted, because the number it predicted was taken and the prediction was
right.

The owning readers allocate a `String` on the host, and a binding that wanted
the bytes somewhere else then copies it there. That is two copies where one
would do. It was recorded here as **a facade cost, not an ABI cost** — a
consequence of this crate's borrow discipline, which returns owned values so
that no borrow survives a call and a runtime re-entering between operations
cannot find a live one — with the note that the same handle ABI over a
write-into-buffer reader would pay one copy.

`blitz-wasm` measured it: reading a 5-byte attribute cost 5 bytes across the
boundary and 5 bytes of host-side `String`, and a 200-byte value that overflowed
the guest's buffer cost 400 bytes of `String` to deliver 200. Then it became the
caller this entry was waiting for, and the variants were added:

| Owning | Buffer-writing |
| --- | --- |
| `element::get_attribute` | `element::get_attribute_into` |
| `node::text_content` | `node::text_content_into`, `node::text_content_len` |

Both sets are supported. **The owning readers are not deprecated**: a caller
that wants a `String` should not have to supply a buffer and then build one, and
`blitz-script` will want exactly that. The buffer variants return the value's
full byte length and write only if it fits, which is `snprintf`'s convention and
the one `blitz-wasm`'s ABI already speaks.

After the change, `blitz-wasm` reports `host_string_bytes == 0` for every read,
with the bytes that cross the boundary unchanged. The prediction held exactly.

Three things are worth carrying forward from it.

**The borrow discipline was never the obstacle.** `element::find_attr` returns
`Option<&str>` into the document, and that is fine because it is `pub(crate)`:
the discipline is a rule about the public surface, and every public caller still
either clones or writes into a caller-supplied buffer.

**`textContent` is not the same shape as an attribute.** An attribute's value is
contiguous in the document, so the buffer variant is one `memcpy` and the
`String` was pure overhead. `textContent` is a concatenation over a subtree that
exists nowhere until something builds it, so the variant removes an
*allocation*, not a copy, and pays for the "nothing is written unless it all
fits" guarantee with a second traversal — one pass to measure, one to fill. A
caller that already knows the length skips the first with `text_content_len`.

**`has_attribute` was the sharpest case and had no buffer at all.** It went
through the same helper as `get_attribute`, so it cloned the whole attribute
value and discarded it to answer a boolean. Zero bytes ever crossed a boundary
for that, which is precisely why a counter watching the boundary could not have
found it. It allocates nothing now.

### The interner is bypassed, so names are re-interned by hashing

`AtomId` and `Interner` exist and the ownership rule is settled (one interner
per document; an id is only valid against the interner that minted it), but the
operations take `&str`. So the binding resolves at its own boundary and hands a
string in, and `markup5ever` then re-interns that string by hashing it.

**No bytes cross the boundary for this**, so counters measuring what the guest
transfers are unaffected. It is a timing tax only: one hash per name per
operation, on names with very small alphabets, exactly where an atom would have
been free.

Atom-taking variants are deliberately deferred. Add them when `blitz-wasm`'s
own numbers justify them, and not before: the shape they should take depends on
what the guest actually passes most, and adding them speculatively fixes a
signature to a caller that does not exist yet.

## Out of scope, and why

Events and event objects, selection, pointer capture, and focus and blur. Each
needs a dispatch model — a listener registry keyed by guest callback, a
propagation path, a notion of what a guest callable is — that belongs with the
runtime binding rather than with a facade over document state. `blitz-script`
implements them in `dom/event.rs`, in `node.rs:638`–`801`, in `document.rs:155`
(`getSelection`), and in `element.rs:149`–`190` and `element.rs:914`–`926`.
They are named as gaps in README.md.

## Not tested here: parity with `blitz-script`

The obvious test for constraint 3 is running an operation through both
implementations and comparing. It is not in this crate, because a
dev-dependency on `blitz-script` puts Boa in this crate's dependency graph and
`tests/no_boa.rs` would fail — the two requirements are in direct conflict.

The right home is `blitz-script`'s own test suite at the point of reparenting,
where both sides are already present and the comparison is against behaviour
that must not change. Until then the guarantee rests on this document and on
the unit tests, which assert the copied edge cases (`insertBefore` with a null
reference, `classList` validation order, detach-not-drop, the comment `data`
quirk) rather than only the happy paths.

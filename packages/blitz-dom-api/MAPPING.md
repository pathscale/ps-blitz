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
| `getBoundingClientRect` | `element.rs:1127` | `geometry::bounding_client_rect` | **different**: does not flush layout. See below |

### `getBoundingClientRect` is the one contract that had to change

Upstream calls `DomCtx::flush_layout` (`state.rs`) first, which resolves the
document only if script has mutated it since the last frame. That dirty flag is
runtime state: it is set by every mutation the *binding* routes through
`mutate_doc`, and cleared by the flush. An operation over a borrowed
`&BaseDocument` has no access to it, and a facade that resolved unconditionally
would turn a cheap read into a full layout pass on every call.

So the flush stays with the caller and the read moves here. A reparented
`blitz-script` keeps its `ctx.flush_layout()` line and replaces only what
follows it. `geometry::bounding_client_rect` documents this, and
`geometry::tests::the_read_does_not_flush_layout` asserts it, so the difference
cannot quietly stop being true.

## CharacterData — `node.rs`

Registered on the CharacterData prototype at `node.rs:87` as `data`, backed by
the same functions as `Node.nodeValue`.

| Operation | Upstream | Facade | Semantics |
| --- | --- | --- | --- |
| `data` | `node.rs:270` | `character_data::data` | identical, including the bug: a comment reports `""` rather than its contents. `clone_node` does copy comment contents, so the two disagree upstream. Preserved so reparenting changes no behaviour; fixing it is a separate decision |
| `data =` | `node.rs:281` | `character_data::set_data` | **different**: does not mark layout dirty |

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

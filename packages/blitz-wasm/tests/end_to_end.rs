//! Build the demo guest to `wasm32-unknown-unknown`, instantiate it under
//! wasmi, run it against a real document, and drive it with real events.
//!
//! Compiling to wasm32 proves the code type-checks. Instantiating proves it
//! links: a missing panic handler, an unsatisfied import, an unsupported `std`
//! surface all show up here and nowhere earlier. So the test builds a real
//! `.wasm` rather than calling the guest crate as a library.
//!
//! And clicking proves the rest. A test that asserted only "dispatch returned
//! OK" would pass against a guest whose handler does nothing at all, so every
//! event test here ends at the DOM: the text node's content, read back out of
//! the document, is the only evidence accepted.

use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

use blitz_dom::{BaseDocument, DocumentConfig, NodeData, NodeId, qual_name};
use blitz_dom_api::{document, element, node};
use blitz_traits::events::DomEvent;
use blitz_traits::shell::{ColorScheme, Viewport};
use blitz_wasm::{Counters, Dispatched, Host, MODULE, OK, dispatch_dom_event};
use keyboard_types::Modifiers;
use wasmi::{Engine, Instance, Linker, Module, Store};

/// Build the demo guest and return the module bytes.
///
/// The guest is a separate workspace with its own target directory. That is
/// not tidiness: `cargo test -p blitz-wasm` holds the lock on this
/// workspace's target directory, so a guest that shared it would deadlock here
/// rather than fail, and a deadlocked test looks like a hung machine.
fn build_guest() -> Vec<u8> {
    // Built once for the whole binary. Every test needs the module, cargo runs
    // them on separate threads, and concurrent `cargo build` invocations
    // against one target directory race: cargo serialises on the lock, but a
    // test can still read the `.wasm` while another invocation is replacing
    // it. That produced exactly one spurious failure under load, which is the
    // worst frequency for a flake to have.
    static GUEST: OnceLock<Vec<u8>> = OnceLock::new();
    GUEST.get_or_init(build_guest_once).clone()
}

fn build_guest_once() -> Vec<u8> {
    let guest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("guest");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());

    let output = Command::new(&cargo)
        .current_dir(&guest_dir)
        .args([
            "build",
            "--release",
            "--target",
            "wasm32-unknown-unknown",
            "--package",
            "blitz-wasm-demo",
        ])
        .output()
        .unwrap_or_else(|err| panic!("could not run `{cargo} build` for the guest: {err}"));

    assert!(
        output.status.success(),
        "guest build failed.\n\
         If this says the wasm32-unknown-unknown target is missing, install it with\n\
         `rustup target add wasm32-unknown-unknown`; this test will not do it for you.\n\
         If it says it could not fetch `solid_rs`, the guest's git dependency needs\n\
         network access on the first build; `Cargo.lock` pins it thereafter.\n\n\
         stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let wasm = guest_dir.join("target/wasm32-unknown-unknown/release/blitz_wasm_demo.wasm");
    std::fs::read(&wasm).unwrap_or_else(|err| panic!("no module at {}: {err}", wasm.display()))
}

/// `<html><body></body></html>` in a document with a viewport, and the body id
/// to mount the guest's tree on.
fn document_with_body() -> (BaseDocument, NodeId) {
    let mut doc = BaseDocument::new(DocumentConfig {
        viewport: Some(Viewport::new(800, 600, 1.0, ColorScheme::Light)),
        ..Default::default()
    });
    let root_id = doc.root_node().id;

    let mut mutr = doc.mutate();
    let html = mutr.create_element(qual_name!("html"), vec![]);
    let body = mutr.create_element(qual_name!("body"), vec![]);
    mutr.append_children(html, &[body]);
    mutr.append_children(root_id, &[html]);
    drop(mutr);

    (doc, body)
}

fn instantiate(bytes: &[u8]) -> (Store<Host>, Instance) {
    let (doc, body) = document_with_body();

    let engine = Engine::default();
    let module = Module::new(&engine, bytes).expect("the guest module should validate");
    let mut store = Store::new(&engine, Host::new(doc, body));
    let mut linker = <Linker<Host>>::new(&engine);
    blitz_wasm::add_to_linker(&mut linker).expect("host functions should register");

    // `instantiate_and_start`, not `instantiate`: the start section runs the
    // module's static initialisers, and a failure there is exactly the class
    // of problem a wasm32 *compile* can never surface.
    let instance = linker
        .instantiate_and_start(&mut store, &module)
        .expect("the guest should instantiate");

    (store, instance)
}

fn call(store: &mut Store<Host>, instance: &Instance, name: &str) -> i32 {
    instance
        .get_typed_func::<(), i32>(&*store, name)
        .unwrap_or_else(|err| panic!("the guest should export `{name}`: {err}"))
        .call(store, ())
        .unwrap_or_else(|err| panic!("`{name}` trapped, which the ABI forbids: {err}"))
}

/// A built, mounted, laid-out counter, ready to be clicked.
///
/// Layout is resolved before returning because a synthetic click reads the
/// node's box to place its coordinates. The event is delivered to an explicit
/// target either way, so an unlaid-out document would still dispatch — it
/// would just dispatch a click at (0, 0), which is a less honest simulation of
/// the thing being tested.
fn counter() -> (Store<Host>, Instance) {
    let bytes = build_guest();
    let (mut store, instance) = instantiate(&bytes);
    let status = call(&mut store, &instance, "run");
    assert_eq!(
        status,
        OK,
        "the guest reported {}: {:?}",
        blitz_wasm::status::name(status),
        store.data().counters().last_dom_error
    );
    store.data_mut().document_mut().resolve(0.0);
    (store, instance)
}

fn query(store: &Store<Host>, selector: &str) -> NodeId {
    document::query_selector(store.data().document(), selector)
        .unwrap()
        .unwrap_or_else(|| panic!("no node matching {selector}"))
}

/// Synthesise a click on the node matching `selector` and drive it all the way
/// through: propagation, the deferred guest dispatch, and the redraw request.
fn click(store: &mut Store<Host>, instance: &Instance, selector: &str) -> Dispatched {
    let target = query(store, selector);
    let event = {
        let doc = store.data().document();
        DomEvent::new(
            target,
            doc.get_node(target)
                .unwrap()
                .synthetic_click_event(Modifiers::empty()),
        )
    };
    dispatch_dom_event(store, instance, event).expect("dispatch should not trap")
}

/// What the counter currently reads, straight out of the document.
fn readout(store: &Store<Host>) -> String {
    node::text_content(store.data().document(), query(store, ".count")).unwrap()
}

fn counters(store: &Store<Host>) -> Counters {
    store.data().counters().clone()
}

/// The whole ABI, in the order this file asserts it.
const ABI: [&str; 11] = [
    "add_listener",
    "append_child",
    "create_element",
    "create_text",
    "get_attribute",
    "has_attribute",
    "intern",
    "remove_listener",
    "set_attribute",
    "set_text",
    "text_content",
];

/// The guest imports nothing outside the ABI, from nowhere but the `blitz`
/// module. No WASI, no JS glue.
///
/// Worth re-asserting now that the guest carries a reactive framework: a
/// scheduler is exactly the sort of thing that reaches for a clock or a
/// microtask queue from the platform, and this is what proves `solid_rs` does
/// not — a `solid_rs` guest's import list is the same eight names as a guest
/// with no framework at all, which is the sharpest statement of what the
/// reactive core costs at the boundary. Nothing.
#[test]
fn the_guest_imports_only_the_blitz_module() {
    let bytes = build_guest();
    let engine = Engine::default();
    let module = Module::new(&engine, &bytes[..]).expect("valid module");

    let mut imports: Vec<String> = module
        .imports()
        .map(|import| format!("{}::{}", import.module(), import.name()))
        .collect();
    imports.sort();

    let abi: Vec<String> = ABI.iter().map(|name| format!("{MODULE}::{name}")).collect();
    for import in &imports {
        assert!(
            abi.contains(import),
            "the guest reached for {import}, which is outside the ABI"
        );
    }

    // Exactly the ten this guest uses: seven to build the counter, three to
    // read it back. The eleventh, `remove_listener`, is registered by the host
    // and declared by the bindings, but this guest never calls it and
    // `lto = true` drops the import — a module imports what it *calls*, not
    // what its bindings declare. Asserting the exact set rather than a subset
    // keeps this test able to notice a guest that quietly stops calling one.
    let expected: Vec<String> = ABI
        .iter()
        .filter(|name| **name != "remove_listener")
        .map(|name| format!("{MODULE}::{name}"))
        .collect();
    assert_eq!(imports, expected);
}

/// The guest exports what the host has to call.
///
/// `dispatch` is the one export the event path needs, and a guest missing it
/// fails at the first click rather than at instantiation — wasm imports are
/// checked when a module is instantiated, exports only when they are looked
/// up. This is the check that turns "the eleventh click panics" into "the
/// guest does not export dispatch".
#[test]
fn the_guest_exports_dispatch() {
    let bytes = build_guest();
    let (store, instance) = instantiate(&bytes);

    instance
        .get_typed_func::<u32, i32>(&store, "dispatch")
        .expect("the guest should export `dispatch(u32) -> i32`");
}

#[test]
fn the_guest_builds_a_tree_through_wasmi() {
    let (store, _instance) = counter();
    let doc = store.data().document();

    let panel = query(&store, ".counter");
    assert_eq!(element::tag_name(doc, panel).unwrap(), "DIV");

    // Mounted under the body the host seeded, not floating detached.
    let body = document::body(doc).unwrap().expect("body");
    assert_eq!(node::parent_node(doc, panel).unwrap(), Some(body));

    let children = node::child_nodes(doc, panel).unwrap();
    assert_eq!(children.len(), 2, "expected a button and a readout");
    assert_eq!(element::tag_name(doc, children[0]).unwrap(), "BUTTON");
    assert_eq!(node::text_content(doc, children[0]).unwrap(), "+1");
    assert_eq!(element::tag_name(doc, children[1]).unwrap(), "SPAN");

    // A text node, not an element with a text-shaped attribute.
    let count_text = node::first_child(doc, children[1]).unwrap().unwrap();
    assert!(matches!(
        doc.get_node(count_text).map(|n| &n.data),
        Some(NodeData::Text(_))
    ));

    // The guest created this node empty; the `0` is the effect's first run, on
    // the flush at the end of `run`. So this assertion is already testing the
    // reactive path, before a single click.
    assert_eq!(readout(&store), "0");

    // One listener, registered on the button.
    assert_eq!(store.data().listeners().len(), 1);
}

/// The whole point of the exercise.
///
/// `click -> host queue -> guest dispatch -> signal write -> microtask drain
/// -> effect -> set_text -> redraw`, with no JavaScript anywhere in it, and
/// asserted at the far end: the text node's content, not the return code.
///
/// This is also the empirical half of the reentrancy claim. The guest's effect
/// calls `set_text` *while the host is mid-dispatch*, which reaches the
/// document through `Caller::data_mut`. If any borrow of that document were
/// still outstanding from propagation, this would not run — it would not have
/// compiled. That the text below changed is the evidence that the borrow was
/// released before the guest was called.
#[test]
fn a_click_increments_the_counter() {
    let (mut store, instance) = counter();
    assert_eq!(readout(&store), "0");

    let dispatched = click(&mut store, &instance, ".increment");
    assert_eq!(
        dispatched,
        Dispatched {
            queued: 1,
            ran: 1,
            failed: 0
        },
        "one listener matched and ran; guest status {:?}",
        store.data().counters().last_guest_status
    );
    assert_eq!(
        readout(&store),
        "1",
        "the click never reached the signal, or the effect never reached the DOM"
    );

    // Twice, because a guest that hardcoded "1" would pass the first
    // assertion. This one only passes if the signal is actually holding state
    // between dispatches.
    click(&mut store, &instance, ".increment");
    assert_eq!(readout(&store), "2");

    // And the mutation was recorded, so an embedder knows to lay out again.
    assert!(store.data().mutated());
    assert!(
        store.data().redraw_requested(),
        "a listener ran, so a frame should have been asked for"
    );
}

/// Mounting the counter, measured. These are the numbers in ABI.md.
///
/// The interned tier is stated in full rather than summarised, because
/// reporting `create_element` and `set_attribute` as free without also
/// reporting what naming cost would be a true number telling a false story.
#[test]
fn mounting_the_counter_costs_the_names_and_the_content() {
    let (store, _instance) = counter();
    let c = counters(&store);

    // The names, each crossing exactly once. Every later use is an integer.
    let names = [
        "div",
        "class",
        "counter",
        "button",
        "increment",
        "span",
        "count",
        "click",
    ];
    let interned: usize = names.iter().map(|name| name.len()).sum();
    assert_eq!(c.intern.calls, 8, "one call per distinct name");
    assert_eq!(c.intern.bytes_copied, interned as u64, "44 bytes of names");
    assert_eq!(store.data().names().len(), 8);

    // The interned tier: three elements, three attributes, five tree edges and
    // one listener, and not one byte between them.
    assert_eq!(c.create_element.calls, 3, "div, button, span");
    assert_eq!(c.create_element.bytes_copied, 0);
    assert_eq!(c.create_element.host_allocs, 0);
    assert_eq!(c.set_attribute.calls, 3);
    assert_eq!(c.set_attribute.bytes_copied, 0);
    assert_eq!(c.set_attribute.host_allocs, 0);
    assert_eq!(c.append_child.calls, 5);
    assert_eq!(c.append_child.bytes_copied, 0);
    assert_eq!(c.add_listener.calls, 1);
    assert_eq!(c.add_listener.bytes_copied, 0);

    // The copied tier: the button's label, the empty node the effect owns,
    // and the `0` the effect put in it. Three bytes, and all three of them are
    // content rather than vocabulary.
    assert_eq!(c.create_text.calls, 2, "`+1`, and the empty count node");
    assert_eq!(c.create_text.bytes_copied, "+1".len() as u64);
    assert_eq!(c.set_text.calls, 1, "the effect's first run");
    assert_eq!(c.set_text.bytes_copied, "0".len() as u64);

    // Read the two totals together: 47 is what a first paint costs including
    // learning the vocabulary, 3 is what it costs once the names are known.
    assert_eq!(c.total_calls(), 23);
    assert_eq!(c.total_bytes_copied(), 47);
    assert_eq!(c.bytes_copied_excluding_interning(), 3);
}

/// A click carries a listener id and nothing else.
///
/// This is the event half of the thesis the crate's byte counters exist to
/// assert. It is stated as a delta rather than a total: what matters is not
/// that the boundary is quiet overall, it is that *the click itself* is free
/// and the only bytes that move are the ones carrying new content.
#[test]
fn a_click_copies_nothing_across_the_boundary() {
    let (mut store, instance) = counter();

    let before = counters(&store);
    click(&mut store, &instance, ".increment");
    let after = counters(&store);

    // The host called into the guest exactly once, and that call carried a
    // `u32`. There is no pointer in `dispatch`'s signature for anything else
    // to travel through, which is why this counter can never be anything else.
    assert_eq!(after.dispatch.calls, before.dispatch.calls + 1);
    assert_eq!(
        after.dispatch.bytes_copied, 0,
        "an event must be a listener id, nothing more"
    );

    // Registering the listener was free too: a handle and an atom.
    assert_eq!(after.add_listener.calls, 1);
    assert_eq!(after.add_listener.bytes_copied, 0);

    // Nothing was interned by the click. "click" crossed once, at setup, and
    // is an integer for the life of the instance.
    assert_eq!(after.intern.calls, before.intern.calls);

    // The only bytes that crossed for the whole click are the one byte of new
    // text the effect wrote: `0` became `1`. Stated as the exact total rather
    // than "small", because "small" is not a number a future change can fail.
    assert_eq!(
        after.total_bytes_copied() - before.total_bytes_copied(),
        "1".len() as u64,
        "the only new information in a click is the digit it produced"
    );
    assert_eq!(after.set_text.calls, before.set_text.calls + 1);
}

/// Ten clicks cost ten digits, and nothing else.
///
/// The steady state a running page is in. The vocabulary was learned once at
/// mount; from here the boundary carries only content, and the per-click cost
/// does not grow with the number of clicks, the size of the tree, or the
/// number of listeners.
#[test]
fn clicking_repeatedly_costs_only_the_digits() {
    let (mut store, instance) = counter();

    let before = counters(&store);
    for _ in 0..10 {
        click(&mut store, &instance, ".increment");
    }
    let after = counters(&store);

    assert_eq!(readout(&store), "10");
    assert_eq!(after.dispatch.calls, before.dispatch.calls + 10);
    assert_eq!(after.dispatch.bytes_copied, 0);
    assert_eq!(
        after.intern.calls, before.intern.calls,
        "nothing new to name"
    );

    // Nine single digits and one "10".
    assert_eq!(
        after.total_bytes_copied() - before.total_bytes_copied(),
        9 + 2,
    );
}

/// A removed listener stops firing, and its id stops being valid.
#[test]
fn a_removed_listener_stops_firing() {
    let (mut store, instance) = counter();
    click(&mut store, &instance, ".increment");
    assert_eq!(readout(&store), "1");

    // Removed from the host side, which is the same table `remove_listener`
    // reaches. The guest's own copy of the handler is irrelevant: with no host
    // registration, propagation matches nothing and the guest is never called.
    store
        .data_mut()
        .listeners_mut()
        .remove(0)
        .expect("the guest registered listener 0");

    let dispatched = click(&mut store, &instance, ".increment");
    assert_eq!(dispatched, Dispatched::default(), "nothing should have run");
    assert_eq!(readout(&store), "1", "the counter should not have moved");

    // And a second removal is an error rather than a quiet success.
    assert_eq!(
        store.data_mut().listeners_mut().remove(0),
        Err(blitz_wasm::ERR_BAD_LISTENER)
    );
}

/// An event nobody listens for queues nothing, calls nobody, and asks for no
/// frame.
#[test]
fn an_unlistened_event_does_not_reach_the_guest() {
    let (mut store, instance) = counter();
    store.data_mut().clear_redraw_request();

    // The readout has no listener on it, and `click` does not bubble to the
    // button from there — they are siblings.
    let dispatched = click(&mut store, &instance, ".count");

    assert_eq!(dispatched, Dispatched::default());
    assert_eq!(store.data().counters().dispatch.calls, 0);
    assert!(
        !store.data().redraw_requested(),
        "nothing ran, so nothing should have asked for a frame"
    );
}

/// A click on the button bubbles to an ancestor listener too, in the order the
/// DOM specifies: target first, then up.
#[test]
fn a_click_bubbles_to_ancestors() {
    let (mut store, instance) = counter();

    // Registered host-side rather than from the guest, because the guest has
    // no handler for it — the point here is the *matching*, and a second
    // listener the guest cannot service would just add a failed dispatch.
    let panel = query(&store, ".counter");
    let click_atom = store
        .data()
        .names()
        .get("click")
        .expect("the guest interned `click` when it registered its listener");
    let ancestor = store
        .data_mut()
        .listeners_mut()
        .add(panel, click_atom)
        .unwrap();

    let dispatched = click(&mut store, &instance, ".increment");
    assert_eq!(
        dispatched.queued, 2,
        "the button's listener, then the panel's"
    );

    // The guest ran both — the second is an id it has no handler for, which it
    // reports rather than trapping on.
    assert_eq!(dispatched.ran, 2);
    assert_eq!(dispatched.failed, 1);
    assert_eq!(store.data().counters().last_guest_status, Some(-7));

    // And the one the guest *does* know still did its job.
    assert_eq!(readout(&store), "1");

    store.data_mut().listeners_mut().remove(ancestor).unwrap();
}

/// The tree the guest built is a real document, so it lays out.
///
/// Without this the test proves only that the right nodes exist, which a tree
/// of detached nodes would also satisfy.
#[test]
fn the_resulting_tree_lays_out() {
    let (mut store, instance) = counter();

    assert!(
        store.data().mutated(),
        "the binding should have recorded that the document changed"
    );
    store.data_mut().clear_mutated();

    let panel = query(&store, ".counter");
    let button = query(&store, ".increment");
    let count = query(&store, ".count");

    let doc = store.data().document();
    let panel_rect = blitz_dom_api::geometry::bounding_client_rect(doc, panel).unwrap();
    assert!(
        panel_rect.width > 0.0 && panel_rect.height > 0.0,
        "the panel should have a box after layout, got {panel_rect:?}"
    );

    // The button and the readout sit side by side, which is what proves they
    // are laid-out children of the panel rather than two nodes that happen to
    // exist.
    let button_rect = blitz_dom_api::geometry::bounding_client_rect(doc, button).unwrap();
    let count_rect = blitz_dom_api::geometry::bounding_client_rect(doc, count).unwrap();
    assert!(
        count_rect.x > button_rect.x,
        "the readout should follow the button, got {button_rect:?} then {count_rect:?}"
    );

    // A click makes the document dirty again, which is how an embedder knows
    // one frame was not enough.
    click(&mut store, &instance, ".increment");
    assert!(store.data().mutated());
    store.data_mut().document_mut().resolve(0.0);
}

// === The read direction ===
//
// Everything above measures the guest writing to the host. Everything below
// measures the host answering the guest, which is the direction the handle and
// atom design does nothing for: the bytes coming back *are* the payload.

/// Run the guest's `echo` export, which reads the tree back and writes what it
/// read into the document.
fn echo(store: &mut Store<Host>, instance: &Instance) {
    let status = call(store, instance, "echo");
    assert_eq!(
        status,
        OK,
        "the guest's read-back reported {status}; last dom error {:?}",
        store.data().counters().last_dom_error
    );
}

/// The text of the node matching `selector`.
fn text_of(store: &Store<Host>, selector: &str) -> String {
    node::text_content(store.data().document(), query(store, selector)).unwrap()
}

/// A read comes back, and the bytes that come back are the right ones.
///
/// Asserted at the DOM, not at the return code. The guest writes what it read
/// straight into the document without formatting it, so a reader that returned
/// nothing shows up here as an empty echo node and a reader that returned the
/// wrong bytes shows up as the wrong echo node. A test that accepted "echo
/// returned 0" would pass against a host reader that answered every call with
/// zero bytes.
#[test]
fn a_read_returns_the_bytes_that_were_written() {
    let (mut store, instance) = counter();
    echo(&mut store, &instance);

    // `class="count"` was set by the guest at mount, as two atoms and zero
    // bytes. It comes back as five bytes.
    assert_eq!(text_of(&store, ".echo-class"), "count");

    // And the subtree's text, concatenated by the host: the button's label and
    // the digit the effect wrote.
    assert_eq!(text_of(&store, ".echo-text"), "+10");
}

/// A read is live, not a cache of what the guest itself last wrote.
#[test]
fn a_read_sees_what_the_host_sees_now() {
    let (mut store, instance) = counter();
    click(&mut store, &instance, ".increment");
    click(&mut store, &instance, ".increment");
    assert_eq!(readout(&store), "2");

    echo(&mut store, &instance);
    assert_eq!(
        text_of(&store, ".echo-text"),
        "+12",
        "the read returned the tree as it was at mount, not as it is now"
    );
}

/// Absent is not empty, and the host and the guest agree on which is which.
///
/// The guest's side of this is the return code — there is no way to make
/// "absent" visible in the DOM without the guest inventing bytes for it — so
/// the host asserts the same distinction independently, against the document
/// the guest was reading.
#[test]
fn an_absent_attribute_is_not_an_empty_one() {
    let (mut store, instance) = counter();

    // `echo` returns OK only if it saw `id` as absent, `data-empty` as present
    // with zero length, `hasAttribute("class")` true and `hasAttribute("id")`
    // false. Any one of those wrong is a distinct negative code.
    echo(&mut store, &instance);

    let doc = store.data().document();
    let readout = query(&store, ".count");
    assert_eq!(element::get_attribute(doc, readout, "id").unwrap(), None);
    assert_eq!(
        element::get_attribute(doc, readout, "data-empty").unwrap(),
        Some(String::new()),
        "the guest set this to empty and read it back as present"
    );

    // `ABSENT` is a status, so nothing about it is an error, and the host's
    // error slot must not have been touched by the absent read.
    assert_eq!(
        store.data().counters().last_error,
        None,
        "an absent attribute is an answer, not a failure"
    );
}

/// What a read costs, stated in full, including the half that never crosses the
/// boundary.
///
/// **The second experiment's result, and it is the one that was hoped for.**
/// The first measurement of this ABI recorded that a read of `n` bytes crossed
/// `n` bytes *and* allocated `n` bytes of host-side `String` first, because
/// every reader in `blitz-dom-api` returned an owned value. That second copy is
/// now gone: the facade's buffer-writing readers put the bytes straight into
/// the guest's buffer, and the `String` never exists.
///
/// The numbers this replaced, for the same page:
///
/// | | before | after |
/// | --- | --- | --- |
/// | `get_attribute.bytes_written` | 5 | 5 |
/// | `get_attribute.host_string_bytes` | 5 | **0** |
/// | `get_attribute.host_allocs` | 2 | **0** |
/// | `text_content.host_string_bytes` | 3 | **0** |
///
/// **What crosses did not change, and that is the point.** The boundary traffic
/// was never the artifact; the host-side copy was, and it was an artifact of the
/// facade rather than of the ABI. `a_read_returns_the_bytes_that_were_written`
/// is what stops that from being a claim about a reader that stopped working.
#[test]
fn a_read_costs_only_what_it_delivers() {
    let (mut store, instance) = counter();
    let before = counters(&store);
    echo(&mut store, &instance);
    let c = counters(&store);

    // Four `get_attribute` calls: `class` on the readout, `id` (absent),
    // `data-empty` (present, zero length), and `data-long` (absent, since this
    // test did not put one there).
    assert_eq!(c.get_attribute.calls, 4);

    // Only `class` had bytes. Five of them crossed, host to guest — unchanged
    // by the experiment, because the payload is the payload.
    assert_eq!(c.get_attribute.bytes_written, "count".len() as u64);
    // And nothing at all was allocated host-side to deliver them. This read
    // 5 before the facade grew `get_attribute_into`.
    assert_eq!(
        c.get_attribute.host_string_bytes, 0,
        "the facade's `String` should be gone, not merely smaller"
    );
    assert_eq!(c.get_attribute.host_allocs, 0);
    // Nothing travelled the other way. A read carries a handle and an atom.
    assert_eq!(c.get_attribute.bytes_copied, 0);

    // `text_content` still rebuilds its answer from the whole subtree every
    // time — that cost is inherent, because the value is a concatenation and
    // exists nowhere until something builds it. What went away is the
    // allocation it used to be built *into*.
    assert_eq!(c.text_content.calls, 1);
    assert_eq!(c.text_content.bytes_written, "+10".len() as u64);
    assert_eq!(c.text_content.host_string_bytes, 0);
    assert_eq!(c.text_content.host_allocs, 0);
    assert_eq!(c.text_content.bytes_copied, 0);

    // `has_attribute` is the one read the atom design does help: a handle and
    // an atom in, a boolean out, no payload in either direction. Its zero used
    // to be a lie of omission — the facade cloned the value and discarded it to
    // answer a boolean — and `element::has_attribute` no longer does that, so
    // the zero is now the whole truth.
    assert_eq!(c.has_attribute.calls, 2);
    assert_eq!(c.has_attribute.bytes_crossed(), 0);
    assert_eq!(c.has_attribute.host_string_bytes, 0);

    // The two directions are separate totals and are never added into one by
    // accident. Nothing before `echo` wrote into guest memory at all, so the
    // read direction's total *is* the delta.
    let read_bytes = c.total_bytes_written() - before.total_bytes_written();
    assert_eq!(before.total_bytes_written(), 0);
    assert_eq!(read_bytes, ("count".len() + "+10".len()) as u64);

    // And the claim itself, as one equality: **a read now costs exactly what it
    // delivers.** Eight bytes of answers, eight bytes across the boundary, and
    // nothing else.
    let read_host_side = (c.get_attribute.host_string_bytes + c.text_content.host_string_bytes)
        - (before.get_attribute.host_string_bytes + before.text_content.host_string_bytes);
    assert_eq!(read_host_side, 0);
    assert!(read_bytes > 0, "a read of nothing would satisfy the above");
}

/// The write direction still pays the second copy, and this is what says so.
///
/// Worth asserting next to the read result rather than assumed: the experiment
/// removed the read direction's host-side `String` and left the write
/// direction's alone, so the two now differ and a future reader should be able
/// to see which is which without re-deriving it.
///
/// `set_text` copies guest memory into a `String` before touching the document
/// because [`blitz_wasm::read_string`] drops its borrow of guest memory first.
/// That was the reentrancy rule as this crate happened to implement it, not as
/// it is required — `host_view` holds both borrows at once and is safe for a
/// stronger reason. So this number is a *remaining* cost, not an inherent one.
#[test]
fn the_write_direction_still_pays_for_its_string() {
    let (store, _instance) = counter();
    let c = counters(&store);

    // The `0` the effect wrote: one byte across, one byte of host `String`.
    assert_eq!(c.set_text.bytes_copied, "0".len() as u64);
    assert_eq!(c.set_text.host_string_bytes, "0".len() as u64);
    assert_eq!(c.set_text.host_allocs, 1);

    // Interning pays it too, and for the same reason.
    assert_eq!(c.intern.host_string_bytes, c.intern.bytes_copied);

    // Reads, by contrast, now pay nothing host-side at all.
    assert_eq!(c.get_attribute.host_string_bytes, 0);
    assert_eq!(c.text_content.host_string_bytes, 0);
}

/// **A read never gets cheaper.**
///
/// This is the sharpest version of the reversal, and the direct mirror of
/// `clicking_repeatedly_costs_only_the_digits`. A repeated *write* is free
/// forever, because the vocabulary was learned once and is integers thereafter.
/// A repeated read of the same unchanged attribute costs the same bytes every
/// time, because there is no place in this design for a returned value to be
/// amortised into. The gap between the two directions widens as a page runs.
#[test]
fn reading_the_same_value_twice_costs_it_twice() {
    let (mut store, instance) = counter();

    echo(&mut store, &instance);
    let first = counters(&store);
    echo(&mut store, &instance);
    let second = counters(&store);

    // Nothing new to name: every string the second pass used was interned by
    // the first, so the write direction really is free the second time round.
    assert_eq!(
        second.intern.calls, first.intern.calls,
        "the second pass should have named nothing new"
    );

    // And the read direction charged full price again, for the same bytes off
    // the same unchanged nodes.
    assert_eq!(
        second.get_attribute.bytes_written - first.get_attribute.bytes_written,
        first.get_attribute.bytes_written
    );
    assert_eq!(
        second.get_attribute.host_string_bytes - first.get_attribute.host_string_bytes,
        first.get_attribute.host_string_bytes
    );
    assert_eq!(
        second.text_content.bytes_written - first.text_content.bytes_written,
        first.text_content.bytes_written
    );
    assert_eq!(
        second.total_bytes_written(),
        2 * first.total_bytes_written()
    );
}

/// The chosen mechanism's failure mode, re-measured after the experiment.
///
/// The guest supplies the buffer, so a value longer than its first guess still
/// costs **two host calls** to deliver once. That is inherent to mechanism (b)
/// and was never going to change: the guest cannot size a buffer for a length
/// it has not been told.
///
/// What did change is the other half. It used to cost **400 bytes of host-side
/// `String` to deliver 200** — the facade built the whole value on the call
/// that wrote nothing, and again on the call that wrote it. Now it costs zero,
/// because neither call builds anything: the first finds the value already
/// contiguous in the document, measures it, and declines to copy.
///
/// So the overflow path's cost is now exactly one extra crossing and one extra
/// attribute lookup, which is the honest floor for "ask without knowing the
/// length".
#[test]
fn a_value_that_does_not_fit_costs_a_second_call_but_no_second_copy() {
    let (mut store, instance) = counter();

    // Longer than the bindings' 64-byte first guess, and set from the host so
    // the guest cannot have known its length in advance.
    let long = "x".repeat(200);
    let readout = query(&store, ".count");
    element::set_attribute(store.data_mut().document_mut(), readout, "data-long", &long).unwrap();

    let before = counters(&store);
    echo(&mut store, &instance);
    let c = counters(&store);

    // It arrived intact, all 200 bytes, through a buffer that started at 64.
    assert_eq!(text_of(&store, ".echo-long"), long);

    // Five calls where the other test saw four: `data-long` took two of them.
    // Unchanged by the experiment, and inherent to the mechanism.
    assert_eq!(c.get_attribute.calls - before.get_attribute.calls, 5);

    // The bytes crossed once. The first call reported the length and wrote
    // nothing, because half a UTF-8 string is not a string.
    assert_eq!(
        c.get_attribute.bytes_written - before.get_attribute.bytes_written,
        ("count".len() + long.len()) as u64
    );

    // And the host built nothing, on either call. This read 405 before the
    // facade grew `get_attribute_into`: the whole 200-byte value, twice, plus
    // the 5 for `class`.
    assert_eq!(
        c.get_attribute.host_string_bytes - before.get_attribute.host_string_bytes,
        0,
        "the overflow path should allocate nothing, not merely less"
    );
    assert_eq!(
        c.get_attribute.host_allocs - before.get_attribute.host_allocs,
        0
    );
}

/// A forged handle and a bad buffer are error returns from a reader, not traps.
///
/// Driven from *inside the guest*, through the real imports, which is the only
/// way to exercise a host function's validation: `add_to_linker`'s closures are
/// not reachable from a test. The guest calls all three readers with a handle
/// this instance never issued, and `get_attribute` with a buffer outside its own
/// memory, and reports what it was told.
#[test]
fn a_forged_read_is_an_error_not_a_trap() {
    let (mut store, instance) = counter();

    let status = call(&mut store, &instance, "probe_forged");
    assert_eq!(
        status, OK,
        "the guest saw an unexpected status from a deliberately bad read; \
         see `probe_forged` in the demo guest for what each code means"
    );

    // Every one of those calls was counted, errors included: a counter that
    // skipped failures would make a guest look cheaper than it is.
    let c = counters(&store);
    assert_eq!(
        c.get_attribute.calls, 2,
        "one forged handle, one bad buffer"
    );
    assert_eq!(c.text_content.calls, 1);
    assert_eq!(c.has_attribute.calls, 1);
    // And none of them produced a byte in either direction.
    assert_eq!(c.get_attribute.bytes_crossed(), 0);
    assert_eq!(c.text_content.bytes_crossed(), 0);
    assert_eq!(
        c.get_attribute.host_string_bytes, 0,
        "nothing was allocated for a call that never reached the document"
    );

    // The instance is still alive, which is the property a trap would have
    // destroyed, and a real read still works.
    echo(&mut store, &instance);
    assert_eq!(text_of(&store, ".echo-class"), "count");
}

/// A guest mistake is a status code, never a trap.
#[test]
fn a_forged_handle_is_an_error_not_a_trap() {
    let (mut store, instance) = counter();

    // Reach past the end of the handle table from the host side, which is the
    // same path a forged guest handle takes.
    let issued = store.data().handles().len();
    assert_eq!(
        store.data().handles().get(issued as u32 + 99),
        Err(blitz_wasm::ERR_BAD_HANDLE)
    );

    // A listener id is a different namespace, and says so.
    assert_eq!(
        store.data_mut().listeners_mut().remove(99),
        Err(blitz_wasm::ERR_BAD_LISTENER)
    );

    // And the instance is still alive and usable afterwards, which is the
    // property a trap would have destroyed.
    click(&mut store, &instance, ".increment");
    assert_eq!(readout(&store), "1");
}

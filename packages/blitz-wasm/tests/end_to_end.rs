//! Build the demo guest to `wasm32-unknown-unknown`, instantiate it under
//! wasmi, run it against a real document, and assert both the resulting tree
//! and the counters.
//!
//! Compiling to wasm32 proves the code type-checks. Instantiating proves it
//! links: a missing panic handler, an unsatisfied import, an unsupported `std`
//! surface all show up here and nowhere earlier. So the test builds a real
//! `.wasm` rather than calling the guest crate as a library.

use std::path::PathBuf;
use std::process::Command;

use blitz_dom::{BaseDocument, DocumentConfig, NodeData, NodeId, qual_name};
use blitz_dom_api::{document, element, node};
use blitz_traits::shell::{ColorScheme, Viewport};
use blitz_wasm::{Host, MODULE, OK};
use wasmi::{Engine, Instance, Linker, Module, Store};

/// Build the demo guest and return the module bytes.
///
/// The guest is a separate workspace with its own target directory. That is
/// not tidiness: `cargo test -p blitz-wasm` holds the lock on this
/// workspace's target directory, so a guest that shared it would deadlock here
/// rather than fail, and a deadlocked test looks like a hung machine.
fn build_guest() -> Vec<u8> {
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
         `rustup target add wasm32-unknown-unknown`; this test will not do it for you.\n\n\
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

/// The guest imports exactly the host functions this crate registers, from the
/// module name it registers them under, and nothing else. No WASI, no JS glue.
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

    assert_eq!(
        imports,
        vec![
            format!("{MODULE}::append_child"),
            format!("{MODULE}::create_element"),
            format!("{MODULE}::create_text"),
            format!("{MODULE}::intern"),
            format!("{MODULE}::set_attribute"),
            format!("{MODULE}::set_text"),
        ],
        "the guest reached for something outside the ABI"
    );
}

#[test]
fn the_guest_builds_a_tree_through_wasmi() {
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

    let host = store.data();
    let doc = host.document();

    // The panel is where the guest put it, with the attributes it set.
    let panel = document::query_selector(doc, ".panel")
        .unwrap()
        .expect("the guest should have created a .panel");
    assert_eq!(
        element::get_attribute(doc, panel, "id").unwrap(),
        Some("root".to_string())
    );
    assert_eq!(element::tag_name(doc, panel).unwrap(), "DIV");

    // Mounted under the body the host seeded, not floating detached.
    let body = document::body(doc).unwrap().expect("body");
    assert_eq!(node::parent_node(doc, panel).unwrap(), Some(body));

    // Heading plus three rows, in order.
    let children = node::child_nodes(doc, panel).unwrap();
    assert_eq!(children.len(), 4, "expected an h1 and three rows");
    assert_eq!(element::tag_name(doc, children[0]).unwrap(), "H1");
    assert_eq!(node::text_content(doc, children[0]).unwrap(), "Blitz");

    let rows = document::query_selector_all(doc, ".row").unwrap();
    assert_eq!(rows, children[1..].to_vec());
    let labels: Vec<String> = rows
        .iter()
        .map(|row| node::text_content(doc, *row).unwrap())
        .collect();
    assert_eq!(labels, vec!["one", "two", "three"]);

    // A text node, not an element with a text-shaped attribute.
    let first_text = node::first_child(doc, rows[0]).unwrap().unwrap();
    assert!(matches!(
        doc.get_node(first_text).map(|n| &n.data),
        Some(NodeData::Text(_))
    ));
}

/// The thesis, as an assertion rather than a claim.
#[test]
fn an_interned_set_attribute_copies_nothing() {
    let bytes = build_guest();
    let (mut store, instance) = instantiate(&bytes);
    assert_eq!(call(&mut store, &instance, "run"), OK);

    let counters = store.data().counters().clone();

    // Five `set_attribute` calls: class and id on the panel, then class on
    // each of the three rows. Not one byte crossed the boundary for any of
    // them, because every argument was a handle or an atom.
    assert_eq!(counters.set_attribute.calls, 5);
    assert_eq!(
        counters.set_attribute.bytes_copied, 0,
        "an interned set_attribute must copy nothing"
    );
    assert_eq!(counters.set_attribute.host_allocs, 0);

    // Same for element creation: the tag is an atom.
    assert_eq!(counters.create_element.calls, 5, "div, h1, and three p");
    assert_eq!(counters.create_element.bytes_copied, 0);

    // And for the tree edges.
    assert_eq!(counters.append_child.bytes_copied, 0);

    // What it did cost, stated rather than hidden. The names crossed once
    // each, at intern time: div, class, panel, id, root, h1, p, row. Reporting
    // set_attribute as free without this line would be a true number telling a
    // false story.
    let interned: usize = ["div", "class", "panel", "id", "root", "h1", "p", "row"]
        .iter()
        .map(|name| name.len())
        .sum();
    assert_eq!(counters.intern.calls, 8, "one call per distinct name");
    assert_eq!(counters.intern.bytes_copied, interned as u64);
    assert_eq!(store.data().names().len(), 8);

    // Text is the other tier and does copy, which is the correct trade: text
    // is content, not a name from a small vocabulary.
    assert_eq!(counters.create_text.calls, 4, "Blitz, one, two, three");
    assert_eq!(
        counters.create_text.bytes_copied,
        ("Blitz".len() + "one".len() + "two".len() + "three".len()) as u64
    );

    // The steady state a running page is in: names already known, so the only
    // bytes crossing are the ones that are genuinely new content.
    assert_eq!(
        counters.bytes_copied_excluding_interning(),
        counters.create_text.bytes_copied
    );
}

/// Handles survive across export calls, and the update path works.
#[test]
fn the_guest_can_come_back_to_a_node_it_kept() {
    let bytes = build_guest();
    let (mut store, instance) = instantiate(&bytes);
    assert_eq!(call(&mut store, &instance, "run"), OK);

    let before = store.data().counters().set_text.calls;
    assert_eq!(call(&mut store, &instance, "update"), OK);
    assert_eq!(store.data().counters().set_text.calls, before + 1);

    let doc = store.data().document();
    let rows = document::query_selector_all(doc, ".row").unwrap();
    let labels: Vec<String> = rows
        .iter()
        .map(|row| node::text_content(doc, *row).unwrap())
        .collect();
    assert_eq!(
        labels,
        vec!["rewritten", "two", "three"],
        "the guest should have rewritten the node it held a handle for"
    );
}

/// The tree the guest built is a real document, so it lays out.
///
/// Without this the test proves only that the right nodes exist, which a tree
/// of detached nodes would also satisfy.
#[test]
fn the_resulting_tree_lays_out() {
    let bytes = build_guest();
    let (mut store, instance) = instantiate(&bytes);
    assert_eq!(call(&mut store, &instance, "run"), OK);

    // The binding tracked that the guest mutated, which is the obligation
    // `blitz-dom-api` leaves to its caller.
    assert!(
        store.data().mutated(),
        "the binding should have recorded that the document changed"
    );

    let host = store.data_mut();
    host.document_mut().resolve(0.0);
    host.clear_mutated();

    let doc = store.data().document();
    let panel = document::query_selector(doc, ".panel").unwrap().unwrap();
    let rect = blitz_dom_api::geometry::bounding_client_rect(doc, panel).unwrap();
    assert!(
        rect.width > 0.0 && rect.height > 0.0,
        "the panel should have a box after layout, got {rect:?}"
    );

    // Each row stacks under the one before it, which is what proves they are
    // block children of the panel rather than four detached nodes that happen
    // to exist.
    let rows = document::query_selector_all(doc, ".row").unwrap();
    let tops: Vec<f64> = rows
        .iter()
        .map(|row| {
            blitz_dom_api::geometry::bounding_client_rect(doc, *row)
                .unwrap()
                .y
        })
        .collect();
    assert!(
        tops.windows(2).all(|pair| pair[1] > pair[0]),
        "rows should stack in order, got tops {tops:?}"
    );
}

/// A guest mistake is a status code, never a trap.
#[test]
fn a_forged_handle_is_an_error_not_a_trap() {
    let bytes = build_guest();
    let (mut store, instance) = instantiate(&bytes);
    assert_eq!(call(&mut store, &instance, "run"), OK);

    // Reach past the end of the handle table from the host side, which is the
    // same path a forged guest handle takes.
    let issued = store.data().handles().len();
    assert!(store.data().handles().get(issued as u32 + 99).is_err());
    assert_eq!(
        store.data().handles().get(issued as u32 + 99),
        Err(blitz_wasm::ERR_BAD_HANDLE)
    );

    // And the instance is still alive and usable afterwards, which is the
    // property a trap would have destroyed.
    assert_eq!(call(&mut store, &instance, "update"), OK);
}

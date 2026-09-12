//! An `on<event>` handler outlives the page's own reference to the element.
//!
//! The JS wrapper cache is weak, and an `on<event>` handler is an ordinary
//! property of the wrapper rather than of the node, so an element the page
//! created, wired up and let go of lost its handler to the collector while it
//! was still in the document. In a browser the element owns that property and
//! the document owns the element, so it survives for as long as the element is
//! in the tree.
//!
//! Every CDN loader is exactly this shape: create a script, set `onload`,
//! append it, return, and wait on the promise the handler resolves. Measured on
//! nofilter.io, whose bootstrap does that: the bundle was fetched and ran, 568
//! nodes were built, and the `load` acknowledgement was dropped because the
//! collector had taken the wrapper during the second the fetch took. The loader
//! then never removed its `body{display:none}` and the site read as one that
//! renders nothing.

use blitz_dom::{Document, DocumentConfig};
use blitz_script::ScriptDocument;
use blitz_traits::events::DomEvent;
use keyboard_types::Modifiers;

fn page(script: &str) -> ScriptDocument {
    ScriptDocument::from_html(
        &format!(
            r#"<html><body>
                 <div id="host"></div>
                 <p id="out">not yet</p>
                 <script>{script}</script>
               </body></html>"#
        ),
        DocumentConfig::default(),
    )
}

fn click(document: &mut ScriptDocument, selector: &str) {
    let (node, event) = {
        let inner = document.inner();
        let node = inner
            .query_selector(selector)
            .unwrap()
            .unwrap_or_else(|| panic!("{selector} is in the document"));
        let event = inner
            .get_node(node)
            .unwrap()
            .synthetic_click_event(Modifiers::empty());
        (node, event)
    };
    document.dispatch_dom_event(DomEvent::new(node, event));
    document.poll(None);
}

fn text_of(document: &ScriptDocument, selector: &str) -> String {
    let inner = document.inner();
    let id = inner.query_selector(selector).unwrap().unwrap();
    inner.get_node(id).unwrap().text_content()
}

/// The element is built, wired and dropped inside a function, so nothing in
/// the page holds it afterwards. Only the document does.
#[test]
fn a_handler_survives_the_page_dropping_the_element() {
    let mut document = page(
        r#"(function () {
             var button = document.createElement('button');
             button.id = 'later';
             button.onclick = function () {
               document.getElementById('out').textContent = 'handled';
             };
             document.getElementById('host').appendChild(button);
           })();"#,
    );
    document.execute_scripts();

    boa_gc::force_collect();
    click(&mut document, "#later");

    assert_eq!(
        text_of(&document, "#out"),
        "handled",
        "the collector took the handler while the element was still in the document"
    );
}

/// A handler assigned after insertion is rooted by the same rule, because the
/// element is reachable through the document either way.
#[test]
fn a_handler_assigned_after_insertion_survives_too() {
    let mut document = page(
        r#"(function () {
             var button = document.createElement('button');
             button.id = 'later';
             document.getElementById('host').appendChild(button);
             button.onclick = function () {
               document.getElementById('out').textContent = 'handled';
             };
           })();"#,
    );
    document.execute_scripts();

    boa_gc::force_collect();
    click(&mut document, "#later");

    assert_eq!(
        text_of(&document, "#out"),
        "handled",
        "a handler set after insertion did not survive collection"
    );
}

/// Solid and other delegated-event runtimes keep the authored callback on an
/// expando property and install one listener on the document. The connected
/// DOM node owns that property even after application code drops its wrapper.
#[test]
fn a_delegated_expando_handler_survives_collection() {
    let mut document = page(
        r#"document.addEventListener('click', function (event) {
             var target = event.composedPath().find(function (node) {
               return node.nodeName === 'BUTTON';
             });
             if (target && target.$$click) target.$$click(event);
           });
           (function () {
             var button = document.createElement('button');
             button.id = 'later';
             button.$$click = function () {
               document.getElementById('out').textContent = 'delegated';
             };
             document.getElementById('host').appendChild(button);
           })();"#,
    );
    document.execute_scripts();

    boa_gc::force_collect();
    click(&mut document, "#later");

    assert_eq!(
        text_of(&document, "#out"),
        "delegated",
        "the collector took a delegated expando while its node was connected"
    );
}

#[test]
fn delegated_handlers_survive_a_framework_remount() {
    let mut document = page(
        r#"document.addEventListener('click', function (event) {
             var target = event.composedPath().find(function (node) {
               return node.nodeName === 'BUTTON';
             });
             if (target && target.$$click) target.$$click(event);
           });
           function render(label) {
             var button = document.createElement('button');
             button.id = label;
             button.textContent = label;
             button.$$click = function () {
               document.getElementById('out').textContent += label;
               if (label === 'first') render('second');
             };
             document.getElementById('host').replaceChildren(button);
           }
           render('first');"#,
    );
    document.execute_scripts();

    boa_gc::force_collect();
    click(&mut document, "#first");
    boa_gc::force_collect();
    click(&mut document, "#second");

    assert_eq!(text_of(&document, "#out"), "not yetfirstsecond");
}

/// A node the page removed and let go of is not kept alive by this. Rooting
/// every wrapper that ever carried a handler would leak exactly the nodes the
/// weak cache exists to release.
#[test]
fn a_removed_element_is_not_rooted_by_its_handler() {
    let mut document = page(
        r#"(function () {
             var button = document.createElement('button');
             button.id = 'later';
             button.onclick = function () {
               document.getElementById('out').textContent = 'handled';
             };
             document.getElementById('host').appendChild(button);
             button.remove();
           })();"#,
    );
    document.execute_scripts();
    document.poll(None);

    boa_gc::force_collect();
    assert!(
        document.inner().query_selector("#later").unwrap().is_none(),
        "a removed element the page no longer holds stayed in the document"
    );
}

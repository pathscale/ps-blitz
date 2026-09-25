//! `MutationObserver`, fed by the document's change log.
//!
//! Before this the only `MutationObserver` a page could see was an embedder's
//! stub that never fired. Frameworks and UI libraries that watch the DOM for
//! a node to appear, an attribute to change, or a subtree to settle waited
//! forever, and nothing said so.
//!
//! Each test mutates in one `eval` and reads what the observer received in the
//! next: delivery happens when a script turn's job queue drains, the way a
//! browser delivers at the end of a microtask checkpoint.

use blitz_script::ScriptDocument;
use serde_json::json;

fn value(doc: &mut ScriptDocument, expression: &str) -> serde_json::Value {
    doc.eval_json(expression).unwrap_or(serde_json::Value::Null)
}

fn page() -> ScriptDocument {
    let mut doc = ScriptDocument::from_html(
        "<html><body><div id='host'><span id='a'>one</span></div></body></html>",
        blitz_dom::DocumentConfig::default(),
    );
    doc.execute_scripts();
    doc.eval(
        "globalThis.seen = [];
         globalThis.host = document.getElementById('host');",
    );
    doc
}

/// An appended child is reported as added to its parent, and the record's
/// node is the very object the script appended.
#[test]
fn an_appended_child_is_reported() {
    let mut doc = page();
    doc.eval(
        "new MutationObserver((records) => {
             for (const r of records) seen.push(r);
         }).observe(host, { childList: true });
         globalThis.added = document.createElement('p');
         host.appendChild(added);",
    );
    assert_eq!(value(&mut doc, "seen.length"), json!(1));
    assert_eq!(value(&mut doc, "seen[0].type"), json!("childList"));
    assert_eq!(value(&mut doc, "seen[0].target === host"), json!(true));
    assert_eq!(
        value(&mut doc, "seen[0].addedNodes[0] === added"),
        json!(true)
    );
    assert_eq!(
        value(
            &mut doc,
            "seen[0].previousSibling === document.getElementById('a')"
        ),
        json!(true)
    );
}

/// Delivery waits for the synchronous code to finish, so a script that
/// mutates and then looks sees nothing yet.
#[test]
fn delivery_is_not_synchronous() {
    let mut doc = page();
    assert_eq!(
        value(
            &mut doc,
            "new MutationObserver((records) => {
                 for (const r of records) seen.push(r);
             }).observe(host, { childList: true });
             host.appendChild(document.createElement('p'));
             seen.length",
        ),
        json!(0)
    );
    assert_eq!(value(&mut doc, "seen.length"), json!(1));
}

/// A removed child is reported with the siblings it sat between.
#[test]
fn a_removed_child_is_reported() {
    let mut doc = page();
    doc.eval(
        "new MutationObserver((records) => {
             for (const r of records) seen.push(r);
         }).observe(host, { childList: true });
         globalThis.gone = document.getElementById('a');
         gone.remove();",
    );
    assert_eq!(value(&mut doc, "seen.length"), json!(1));
    assert_eq!(
        value(&mut doc, "seen[0].removedNodes[0] === gone"),
        json!(true)
    );
    assert_eq!(value(&mut doc, "seen[0].addedNodes.length"), json!(0));
}

/// An attribute change carries the old value when asked for, and a filter
/// keeps other attributes out.
#[test]
fn attributes_honour_old_value_and_filter() {
    let mut doc = page();
    doc.eval(
        "host.setAttribute('data-state', 'closed');
         new MutationObserver((records) => {
             for (const r of records) seen.push(r);
         }).observe(host, { attributeFilter: ['data-state'], attributeOldValue: true });
         host.setAttribute('data-state', 'open');
         host.setAttribute('title', 'ignored');",
    );
    assert_eq!(value(&mut doc, "seen.length"), json!(1));
    assert_eq!(value(&mut doc, "seen[0].type"), json!("attributes"));
    assert_eq!(
        value(&mut doc, "seen[0].attributeName"),
        json!("data-state")
    );
    assert_eq!(value(&mut doc, "seen[0].oldValue"), json!("closed"));
}

/// Without `attributeOldValue` the old value is withheld, as in a browser.
#[test]
fn old_values_are_withheld_unless_asked_for() {
    let mut doc = page();
    doc.eval(
        "host.className = 'a';
         new MutationObserver((records) => {
             for (const r of records) seen.push(r);
         }).observe(host, { attributes: true });
         host.className = 'b';",
    );
    assert_eq!(value(&mut doc, "seen.length"), json!(1));
    assert_eq!(value(&mut doc, "seen[0].attributeName"), json!("class"));
    assert_eq!(value(&mut doc, "seen[0].oldValue"), json!(null));
}

/// Text changes are character data, reported on the text node itself.
#[test]
fn text_changes_are_character_data() {
    let mut doc = page();
    doc.eval(
        "globalThis.text = document.getElementById('a').firstChild;
         new MutationObserver((records) => {
             for (const r of records) seen.push(r);
         }).observe(host, { characterData: true, characterDataOldValue: true, subtree: true });
         text.data = 'two';",
    );
    assert_eq!(value(&mut doc, "seen.length"), json!(1));
    assert_eq!(value(&mut doc, "seen[0].type"), json!("characterData"));
    assert_eq!(value(&mut doc, "seen[0].target === text"), json!(true));
    assert_eq!(value(&mut doc, "seen[0].oldValue"), json!("one"));
}

/// Without `subtree` a change below the observed node is not reported; with
/// it, it is.
#[test]
fn subtree_decides_whether_descendants_count() {
    let mut doc = page();
    doc.eval(
        "globalThis.shallow = [];
         new MutationObserver((records) => {
             for (const r of records) shallow.push(r);
         }).observe(host, { attributes: true });
         new MutationObserver((records) => {
             for (const r of records) seen.push(r);
         }).observe(host, { attributes: true, subtree: true });
         document.getElementById('a').setAttribute('title', 'x');",
    );
    assert_eq!(value(&mut doc, "shallow.length"), json!(0));
    assert_eq!(value(&mut doc, "seen.length"), json!(1));
}

/// A fragment built while detached and then inserted is one change, not one
/// per node it was built from.
#[test]
fn an_inserted_fragment_is_one_record() {
    let mut doc = page();
    doc.eval(
        "new MutationObserver((records) => {
             for (const r of records) seen.push(r);
         }).observe(document.body, { childList: true, subtree: true });
         const fragment = document.createDocumentFragment();
         const list = document.createElement('ul');
         for (let i = 0; i < 3; i++) list.appendChild(document.createElement('li'));
         fragment.appendChild(list);
         host.appendChild(fragment);",
    );
    assert_eq!(value(&mut doc, "seen.length"), json!(1));
    assert_eq!(
        value(&mut doc, "seen[0].addedNodes[0].tagName"),
        json!("UL")
    );
}

/// After `disconnect` nothing more arrives.
#[test]
fn disconnect_stops_delivery() {
    let mut doc = page();
    doc.eval(
        "globalThis.observer = new MutationObserver((records) => {
             for (const r of records) seen.push(r);
         });
         observer.observe(host, { childList: true });
         host.appendChild(document.createElement('p'));",
    );
    assert_eq!(value(&mut doc, "seen.length"), json!(1));
    doc.eval(
        "observer.disconnect();
         host.appendChild(document.createElement('p'));",
    );
    assert_eq!(value(&mut doc, "seen.length"), json!(1));
}

/// The observer must set at least one of the three kinds, as a browser
/// requires; an empty options object is a `TypeError`, not silence.
#[test]
fn empty_options_are_refused() {
    let mut doc = page();
    assert_eq!(
        value(
            &mut doc,
            "try { new MutationObserver(() => {}).observe(host, {}); 'accepted' }
             catch (error) { error instanceof TypeError ? 'TypeError' : String(error) }",
        ),
        json!("TypeError")
    );
}

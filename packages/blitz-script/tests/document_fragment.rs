//! `document.createDocumentFragment`, and what a fragment does when inserted.
//!
//! jQuery builds one during initialisation:
//!
//! ```js
//! xe = C.createDocumentFragment().appendChild(C.createElement("div"))
//! ```
//!
//! Without the method that threw `TypeError: not a callable function`, and the
//! library died before defining `jQuery`. Eight sites in a hundred-site corpus
//! failed exactly there — across four jQuery versions on four different CDNs —
//! and every one reported it downstream as `jQuery is not defined`, a missing
//! global that was never missing. The method that was actually absent says
//! nothing about itself in the error, which is why it went unranked.

use blitz_script::ScriptDocument;

fn value(doc: &mut ScriptDocument, expression: &str) -> serde_json::Value {
    doc.eval_json(expression).unwrap_or(serde_json::Value::Null)
}

fn page() -> ScriptDocument {
    ScriptDocument::from_html(
        "<html><body><div id='host'></div></body></html>",
        blitz_dom::DocumentConfig::default(),
    )
}

/// The method exists and returns a node.
#[test]
fn a_fragment_can_be_created() {
    let mut doc = page();
    doc.execute_scripts();

    assert_eq!(
        value(&mut doc, "typeof document.createDocumentFragment"),
        serde_json::json!("function")
    );
}

/// It identifies as a fragment, not as an element.
///
/// A stand-in element would answer 1 and its tag name here, and code that
/// branches on either would take the wrong path without saying so.
#[test]
fn a_fragment_reports_itself_as_one() {
    let mut doc = page();
    doc.execute_scripts();
    doc.eval("globalThis.f = document.createDocumentFragment();");

    assert_eq!(value(&mut doc, "f.nodeType"), serde_json::json!(11));
    assert_eq!(
        value(&mut doc, "f.nodeName"),
        serde_json::json!("#document-fragment")
    );
}

/// jQuery's exact idiom: append to a fragment, keep the child.
#[test]
fn appending_to_a_fragment_returns_the_child() {
    let mut doc = page();
    doc.execute_scripts();
    doc.eval(
        "globalThis.kept = document.createDocumentFragment()
             .appendChild(document.createElement('div'));",
    );

    assert_eq!(value(&mut doc, "kept.nodeName"), serde_json::json!("DIV"));
}

/// Inserting a fragment inserts its children, not the fragment.
///
/// This is the assertion a detached-element stand-in fails, and the reason one
/// was not shipped: it would insert itself and leave a wrapper in the document
/// that the page never asked for.
#[test]
fn inserting_a_fragment_inserts_its_children() {
    let mut doc = page();
    doc.execute_scripts();
    doc.eval(
        "var f = document.createDocumentFragment();
         f.appendChild(document.createElement('span'));
         f.appendChild(document.createElement('em'));
         globalThis.before = f.childNodes.length;
         document.getElementById('host').appendChild(f);",
    );

    let host_children = value(
        &mut doc,
        "document.getElementById('host').childNodes.length",
    );
    let names = value(
        &mut doc,
        "Array.prototype.map.call(
             document.getElementById('host').childNodes, function (n) { return n.nodeName; }
         ).join(',')",
    );

    assert_eq!(
        host_children,
        serde_json::json!(2),
        "both children should have moved into the host"
    );
    assert_eq!(names, serde_json::json!("SPAN,EM"));

    // And the fragment is emptied, as the spec requires. Leaving the children
    // behind would give each of them two parents.
    assert_eq!(
        value(&mut doc, "f.childNodes.length"),
        serde_json::json!(0),
        "the fragment should be empty after insertion"
    );
}

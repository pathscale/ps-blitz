//! `document.implementation.createHTMLDocument`.
//!
//! jQuery runs this during initialisation, before it assigns itself to
//! `window`:
//!
//! ```js
//! le.createHTMLDocument = ((Jt = C.implementation.createHTMLDocument("").body)
//!     .innerHTML = "<form></form><form></form>", 2 === Jt.childNodes.length)
//! ```
//!
//! `document.implementation` is `undefined`, so reading `createHTMLDocument`
//! off it throws `TypeError: cannot convert 'null' or 'undefined' to object`
//! and the library never defines `jQuery`. Every site hitting it reports the
//! downstream symptom, `jQuery is not defined` — a global that was never
//! missing — which is why the cause went unranked for so long.
//!
//! These tests are the three lines of that feature-detect and nothing else.
//! Loading the real library to prove the same thing means carrying 87KB of
//! minified script to assert `childNodes.length == 2`.

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

/// `document.implementation` exists.
#[test]
fn implementation_is_an_object() {
    let mut doc = page();
    doc.execute_scripts();
    assert_eq!(value(&mut doc, "typeof document.implementation"), "object");
}

/// It carries `createHTMLDocument`.
#[test]
fn create_html_document_is_callable() {
    let mut doc = page();
    doc.execute_scripts();
    assert_eq!(
        value(
            &mut doc,
            "typeof document.implementation.createHTMLDocument"
        ),
        "function"
    );
}

/// The returned document has a `body`, and it is not this document's body.
///
/// `document.body` resolves by searching for the first `body` in the tree
/// rather than within the document it was asked of, so a second document is
/// exactly the case that catches it. A `createHTMLDocument` that returned the
/// live page's body would pass a test that only checked `body` was non-null,
/// and would then let jQuery scribble two `<form>` elements into the page.
#[test]
fn the_new_document_has_its_own_body() {
    let mut doc = page();
    doc.execute_scripts();

    assert_eq!(
        value(
            &mut doc,
            "(function () {\
               var d = document.implementation.createHTMLDocument('');\
               return d.body ? 'present' : 'missing';\
             })()"
        ),
        "present"
    );
    assert_eq!(
        value(
            &mut doc,
            "(function () {\
               var d = document.implementation.createHTMLDocument('');\
               return d.body === document.body;\
             })()"
        ),
        false,
        "the new document handed back the live page's body"
    );
}

/// The title argument is applied.
#[test]
fn the_title_argument_is_used() {
    let mut doc = page();
    doc.execute_scripts();
    assert_eq!(
        value(
            &mut doc,
            "document.implementation.createHTMLDocument('hello').title"
        ),
        "hello"
    );
}

/// jQuery's feature-detect, exactly.
///
/// This is the assertion the whole thing exists for: two `<form>` elements
/// parsed into the new document's body come back as two children. jQuery reads
/// the result to decide whether it can use `createHTMLDocument` for parsing.
#[test]
fn the_jquery_feature_detect_answers_two() {
    let mut doc = page();
    doc.execute_scripts();
    assert_eq!(
        value(
            &mut doc,
            "(function () {\
               var b = document.implementation.createHTMLDocument('').body;\
               b.innerHTML = '<form></form><form></form>';\
               return b.childNodes.length;\
             })()"
        ),
        2
    );
}

/// Writing into the new document does not touch the live one.
///
/// The same arena backs both, so this is the failure mode to guard: a
/// `createHTMLDocument` implemented by appending to the real tree would pass
/// every test above and corrupt the page.
#[test]
fn the_live_document_is_untouched() {
    let mut doc = page();
    doc.execute_scripts();
    assert_eq!(
        value(
            &mut doc,
            "(function () {\
               var before = document.body.childNodes.length;\
               var b = document.implementation.createHTMLDocument('').body;\
               b.innerHTML = '<form></form><form></form>';\
               return document.body.childNodes.length === before;\
             })()"
        ),
        true,
        "creating a document changed the live page"
    );
}

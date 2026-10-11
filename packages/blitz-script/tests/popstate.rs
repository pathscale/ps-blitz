//! `history.back` and `history.forward` must dispatch `popstate`.
//!
//! Without it a router's `navigate(-1)` changed the URL and nothing redrew, so
//! every route was one-way.

use blitz_dom::{Document, DocumentConfig};
use blitz_script::ScriptDocument;

fn doc_from_html(html: &str) -> ScriptDocument {
    // A real viewport: a zero-width one gives every box a zero content size, so
    // nothing overflows and nothing scrolls.
    let mut doc = ScriptDocument::from_html(
        html,
        DocumentConfig {
            // A relative route needs a hierarchical document URL.
            // The default about:blank URL cannot resolve "/b".
            base_url: Some("https://example.invalid/".to_owned()),
            viewport: Some(blitz_traits::shell::Viewport::new(
                800,
                600,
                1.0,
                blitz_traits::shell::ColorScheme::Light,
            )),
            ..Default::default()
        },
    );
    doc.execute_scripts();
    doc
}

fn text_of_selector(doc: &ScriptDocument, selector: &str) -> String {
    let inner = doc.inner();
    let node_id = inner
        .query_selector(selector)
        .unwrap()
        .unwrap_or_else(|| panic!("no node matching {selector}"));
    inner.get_node(node_id).unwrap().text_content()
}

#[test]
fn history_back_dispatches_popstate() {
    let mut doc = doc_from_html(
        r#"
        <html><body>
            <div id="out"></div>
            <script>
                const out = document.getElementById("out");
                addEventListener("popstate", (event) => {
                    out.textContent += "pop:" + JSON.stringify(event.state) + "|";
                    if (event.state === null) history.forward();
                });
                history.pushState({ route: "b" }, "", "/b");
                history.back();
            </script>
        </body></html>
        "#,
    );

    // Drain both the startup traversal and the traversal its callback queues.
    doc.run_until_idle().expect("script startup completes");

    assert_eq!(
        text_of_selector(&doc, "#out"),
        "pop:null|pop:{\"route\":\"b\"}|",
        "a traversal must announce itself, or a router never redraws"
    );
}

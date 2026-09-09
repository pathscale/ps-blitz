//! `Event.cancelBubble`, the legacy alias for the stop-propagation flag.
//!
//! Absent from both the shim and the bundled framework, so a delegated
//! dispatcher had nothing to read and `stopPropagation` did not hold. Measured
//! on two sites: pressing a cookie-preferences category dismissed the whole
//! dialog, because the closing backdrop is an ancestor of the panel.

use blitz_dom::{Document, DocumentConfig};
use blitz_script::ScriptDocument;
use blitz_traits::events::{DomEvent, DomEventData};

fn doc_from_html(html: &str) -> ScriptDocument {
    // A real viewport: a zero-width one gives every box a zero content size, so
    // nothing overflows and nothing scrolls.
    let mut doc = ScriptDocument::from_html(
        html,
        DocumentConfig {
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
fn cancel_bubble_reads_and_writes_the_stop_propagation_flag() {
    let mut doc = doc_from_html(
        r#"
        <html><body>
            <div id="backdrop"><button id="category">Analytics</button></div>
            <div id="out"></div>
            <script>
                const out = document.getElementById("out");
                document.getElementById("category").addEventListener("click", (event) => {
                    event.stopPropagation();
                    out.textContent += "category:" + event.cancelBubble + "|";
                });
                document.getElementById("backdrop").addEventListener("click", () => {
                    out.textContent += "backdrop|";
                });
            </script>
        </body></html>
        "#,
    );

    let (category, pointer) = {
        let inner = doc.inner();
        let category = inner.query_selector("#category").unwrap().unwrap();
        let pointer = match inner
            .get_node(category)
            .unwrap()
            .synthetic_click_event(keyboard_types::Modifiers::empty())
        {
            DomEventData::Click(pointer) => pointer,
            _ => unreachable!(),
        };
        (category, pointer)
    };
    doc.dispatch_dom_event(DomEvent::new(category, DomEventData::Click(pointer)));

    assert_eq!(
        text_of_selector(&doc, "#out"),
        "category:true|",
        "cancelBubble must report the stop-propagation flag, and the ancestor must not run"
    );
}

#[test]
fn setting_cancel_bubble_stops_propagation() {
    let mut doc = doc_from_html(
        r#"
        <html><body>
            <div id="backdrop"><button id="category">Analytics</button></div>
            <div id="out"></div>
            <script>
                const out = document.getElementById("out");
                document.getElementById("category").addEventListener("click", (event) => {
                    event.cancelBubble = true;
                    out.textContent += "category|";
                });
                document.getElementById("backdrop").addEventListener("click", () => {
                    out.textContent += "backdrop|";
                });
            </script>
        </body></html>
        "#,
    );

    let (category, pointer) = {
        let inner = doc.inner();
        let category = inner.query_selector("#category").unwrap().unwrap();
        let pointer = match inner
            .get_node(category)
            .unwrap()
            .synthetic_click_event(keyboard_types::Modifiers::empty())
        {
            DomEventData::Click(pointer) => pointer,
            _ => unreachable!(),
        };
        (category, pointer)
    };
    doc.dispatch_dom_event(DomEvent::new(category, DomEventData::Click(pointer)));

    assert_eq!(
        text_of_selector(&doc, "#out"),
        "category|",
        "the backdrop is an ancestor of the panel, so a dismiss handler on it must not run"
    );
}

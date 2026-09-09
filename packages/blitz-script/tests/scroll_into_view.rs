//! `scrollIntoView` on an element, which blitz-dom has had all along as
//! `scroll_to_node` but never exposed to page script. A page calling it got
//! "TypeError: not a callable function", which is what an anchor-scrolling
//! router, a "back to top" control and a validation-error focuser all call.

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
fn scroll_into_view_is_callable_from_page_script() {
    let mut doc = doc_from_html(
        r#"
        <html><body style="margin:0">
            <div id="pane" style="height:100px; overflow:scroll">
                <div style="height:400px"></div>
                <div id="target" style="height:20px">target</div>
            </div>
            <button id="jump">jump</button>
            <div id="out"></div>
            <script>
                const out = document.getElementById("out");
                document.getElementById("jump").addEventListener("click", () => {
                    try {
                        document.getElementById("target").scrollIntoView({ behavior: "instant" });
                        out.textContent = "called";
                    } catch (error) {
                        out.textContent = "threw:" + error;
                    }
                });
            </script>
        </body></html>
        "#,
    );
    doc.inner_mut().resolve(0.0);

    let (jump, pointer) = {
        let inner = doc.inner();
        let jump = inner.query_selector("#jump").unwrap().unwrap();
        let pointer = match inner
            .get_node(jump)
            .unwrap()
            .synthetic_click_event(keyboard_types::Modifiers::empty())
        {
            DomEventData::Click(pointer) => pointer,
            _ => unreachable!(),
        };
        (jump, pointer)
    };
    doc.dispatch_dom_event(DomEvent::new(jump, DomEventData::Click(pointer)));

    assert_eq!(text_of_selector(&doc, "#out"), "called");
    let scrolled = {
        let inner = doc.inner();
        let pane = inner.query_selector("#pane").unwrap().unwrap();
        inner.get_node(pane).unwrap().scroll_offset().y
    };
    assert!(
        scrolled > 0.0,
        "scrollIntoView must move the nearest scroll container, not just return"
    );
}

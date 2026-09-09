//! `change` on a text control.
//!
//! It was synthesised only for checkbox and radio, so any `onChange` on a text
//! field was dead. That shipped a product bug: a phone-number field bound to
//! `change` gated a Confirm button that nothing could ever open.

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
fn a_text_input_fires_change_when_it_loses_focus_with_a_new_value() {
    let mut doc = doc_from_html(
        r#"
        <html><body>
            <input id="phone" type="tel">
            <div id="out"></div>
            <script>
                const out = document.getElementById("out");
                document.getElementById("phone").addEventListener("change", (event) => {
                    out.textContent += "change:" + event.target.value + "|";
                });
                document.getElementById("phone").addEventListener("input", () => {
                    out.textContent += "input|";
                });
            </script>
        </body></html>
        "#,
    );

    doc.inner_mut().resolve(0.0);
    let phone = doc.inner().query_selector("#phone").unwrap().unwrap();
    doc.dispatch_dom_event(DomEvent::new(
        phone,
        DomEventData::Focus(blitz_traits::events::BlitzFocusEvent),
    ));
    doc.inner_mut().mutate().set_attribute(
        phone,
        markup5ever::QualName::new(None, markup5ever::ns!(), markup5ever::local_name!("value")),
        "555",
    );
    doc.dispatch_dom_event(DomEvent::new(
        phone,
        DomEventData::Input(blitz_traits::events::BlitzInputEvent {
            value: "555".to_string(),
        }),
    ));
    doc.dispatch_dom_event(DomEvent::new(
        phone,
        DomEventData::Blur(blitz_traits::events::BlitzFocusEvent),
    ));

    assert_eq!(
        text_of_selector(&doc, "#out"),
        "input|change:555|",
        "a text field must commit a change event, or an onChange gate can never open"
    );
}

#[test]
fn an_unedited_text_input_does_not_fire_change_on_blur() {
    let mut doc = doc_from_html(
        r#"
        <html><body>
            <input id="phone" type="tel" value="555">
            <div id="out"></div>
            <script>
                const out = document.getElementById("out");
                document.getElementById("phone").addEventListener("change", () => {
                    out.textContent += "change|";
                });
            </script>
        </body></html>
        "#,
    );

    doc.inner_mut().resolve(0.0);
    let phone = doc.inner().query_selector("#phone").unwrap().unwrap();
    doc.dispatch_dom_event(DomEvent::new(
        phone,
        DomEventData::Focus(blitz_traits::events::BlitzFocusEvent),
    ));
    doc.dispatch_dom_event(DomEvent::new(
        phone,
        DomEventData::Blur(blitz_traits::events::BlitzFocusEvent),
    ));

    assert_eq!(
        text_of_selector(&doc, "#out"),
        "",
        "change is a commit, not a blur notification"
    );
}

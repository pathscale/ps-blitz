//! `change` on a `<select>`.
//!
//! `change` was synthesised only for checkbox and radio, so a picker fired
//! nothing at all when the user chose something. A select commits immediately
//! rather than on blur, like a checkbox and unlike a text control: the
//! keystroke is the commit.

use blitz_dom::{Document, DocumentConfig};
use blitz_script::ScriptDocument;
use blitz_traits::events::{BlitzKeyEvent, KeyState, UiEvent};
use keyboard_types::{Code, Key, Location, Modifiers};

fn doc_from_html(html: &str) -> ScriptDocument {
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
    // Resolve before the scripts run: the live selectedness a select reports
    // is created by layout construction.
    doc.inner_mut().resolve(0.0);
    doc.execute_scripts();
    doc
}

fn text_of(doc: &ScriptDocument, selector: &str) -> String {
    let inner = doc.inner();
    let node_id = inner
        .query_selector(selector)
        .unwrap()
        .unwrap_or_else(|| panic!("no node matching {selector}"));
    inner.get_node(node_id).unwrap().text_content()
}

fn key(key: Key, state: KeyState) -> BlitzKeyEvent {
    BlitzKeyEvent {
        key: key.clone(),
        code: match key {
            Key::ArrowDown => Code::ArrowDown,
            Key::ArrowUp => Code::ArrowUp,
            _ => Code::Unidentified,
        },
        modifiers: Modifiers::empty(),
        location: Location::Standard,
        is_auto_repeating: false,
        is_composing: false,
        state,
        text: None,
    }
}

const PAGE: &str = r#"
    <html><body>
        <select id="country">
            <option value="us">United States</option>
            <option value="fr">France</option>
        </select>
        <div id="out"></div>
        <script>
            const out = document.getElementById("out");
            const select = document.getElementById("country");
            select.addEventListener("input", () => { out.textContent += "input|"; });
            select.addEventListener("change", (event) => {
                out.textContent += "change:" + event.target.value + "|";
            });
        </script>
    </body></html>
"#;

fn press_arrow_down(doc: &mut ScriptDocument) {
    let select = doc.inner().query_selector("#country").unwrap().unwrap();
    doc.inner_mut().set_focus_to(select);
    doc.handle_ui_event(UiEvent::KeyDown(key(Key::ArrowDown, KeyState::Pressed)));
    doc.handle_ui_event(UiEvent::KeyUp(key(Key::ArrowDown, KeyState::Released)));
}

#[test]
fn a_keyboard_selection_fires_input_and_then_change() {
    let mut doc = doc_from_html(PAGE);
    press_arrow_down(&mut doc);

    assert_eq!(
        text_of(&doc, "#out"),
        "input|change:fr|",
        "a select commits on selection, so change must follow input immediately"
    );
}

#[test]
fn a_keystroke_that_changes_nothing_fires_nothing() {
    let mut doc = doc_from_html(PAGE);
    // Already on the last option after one press, so the second has nowhere to
    // go: a select does not wrap, and an event for a selection that did not
    // move would make every `change` listener fire on a no-op.
    press_arrow_down(&mut doc);
    press_arrow_down(&mut doc);

    assert_eq!(text_of(&doc, "#out"), "input|change:fr|");
}

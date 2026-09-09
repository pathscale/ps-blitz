//! Pressing Enter in a form control must not dereference a control that has
//! been removed, and must submit when the form has a submit button.
//!
//! Two separate defects, both on the same walk of `controls_to_form`:
//!
//! 1. Setting a field's value re-creates its input node. The old id stayed in
//!    `controls_to_form`, and `implicit_form_submission` indexed the slab for
//!    every control it found there, so the second value-set followed by Enter
//!    panicked with "invalid SlotMap key used" and took the host down with it.
//!
//! 2. The "more than one field that blocks implicit submission" rule is gated
//!    on the form having no submit button
//!    (<https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#implicit-submission>).
//!    Applying it unconditionally meant Enter never submitted any form with two
//!    explicitly typed fields, submit button or not. Three sites hit this.

use blitz_dom::{Document, DocumentConfig};
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_traits::{
    navigation::{NavigationOptions, NavigationProvider},
    shell::{ColorScheme, Viewport},
};
use keyboard_types::{Code, Key, Location, Modifiers};

use std::sync::{Arc, Mutex};

#[derive(Default)]
struct RecordingNavigation {
    navigations: Mutex<Vec<String>>,
}

impl NavigationProvider for RecordingNavigation {
    fn navigate_to(&self, options: NavigationOptions) {
        self.navigations
            .lock()
            .unwrap()
            .push(options.url.to_string());
    }
}

fn doc_with(html: &str, navigation: Arc<RecordingNavigation>) -> HtmlDocument {
    HtmlDocument::from_html(
        html,
        DocumentConfig {
            viewport: Some(Viewport::new(800, 600, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            navigation_provider: Some(navigation),
            base_url: Some("https://example.test/page".to_string()),
            ..Default::default()
        },
    )
}

fn press_enter(doc: &mut HtmlDocument, node_id: blitz_traits::node_id::NodeId) {
    doc.set_focus_to(node_id);
    doc.handle_ui_event(blitz_traits::events::UiEvent::KeyDown(
        blitz_traits::events::BlitzKeyEvent {
            key: Key::Enter,
            code: Code::Enter,
            modifiers: Modifiers::empty(),
            location: Location::Standard,
            is_auto_repeating: false,
            is_composing: false,
            state: blitz_traits::events::KeyState::Pressed,
            text: None,
        },
    ));
}

const REBUILT: &str = r#"<!DOCTYPE html>
<html><body>
    <form id="form" action="/search" method="get">
        <input id="q" name="q">
    </form>
</body></html>
"#;

#[test]
fn enter_after_two_value_sets_does_not_panic_on_a_recreated_control() {
    let navigation = Arc::new(RecordingNavigation::default());
    let mut doc = doc_with(REBUILT, Arc::clone(&navigation));
    doc.resolve(0.0);

    let form = doc.query_selector("#form").unwrap().expect("form");

    // Setting the field's value through a framework re-renders the form and
    // gives the input a new node. Twice, because the first pass is what leaves
    // the dead id behind and the second is what walks past it.
    for value in ["one", "two"] {
        let mut mutator = doc.mutate();
        mutator.set_inner_html(form, &format!(r#"<input id="q" name="q" value="{value}">"#));
        drop(mutator);
        doc.resolve(0.0);
    }

    let input = doc.query_selector("#q").unwrap().expect("input");
    press_enter(&mut doc, input);

    assert_eq!(
        navigation.navigations.lock().unwrap().len(),
        1,
        "Enter in a single-field form must still submit it"
    );
}

const TWO_TYPED_FIELDS_WITH_SUBMIT: &str = r#"<!DOCTYPE html>
<html><body>
    <form id="form" action="/login" method="get">
        <input id="user" name="user" type="text">
        <input id="pass" name="pass" type="password">
        <button type="submit">Sign in</button>
    </form>
</body></html>
"#;

#[test]
fn enter_submits_a_multi_field_form_that_has_a_submit_button() {
    let navigation = Arc::new(RecordingNavigation::default());
    let mut doc = doc_with(TWO_TYPED_FIELDS_WITH_SUBMIT, Arc::clone(&navigation));
    doc.resolve(0.0);

    let user = doc.query_selector("#user").unwrap().expect("user field");
    press_enter(&mut doc, user);

    assert_eq!(
        navigation.navigations.lock().unwrap().len(),
        1,
        "the spec gates the multi-field rule on the form having no submit button"
    );
}

const TWO_TYPED_FIELDS_NO_SUBMIT: &str = r#"<!DOCTYPE html>
<html><body>
    <form id="form" action="/login" method="get">
        <input id="user" name="user" type="text">
        <input id="pass" name="pass" type="password">
    </form>
</body></html>
"#;

#[test]
fn enter_does_not_submit_a_multi_field_form_with_no_submit_button() {
    let navigation = Arc::new(RecordingNavigation::default());
    let mut doc = doc_with(TWO_TYPED_FIELDS_NO_SUBMIT, Arc::clone(&navigation));
    doc.resolve(0.0);

    let user = doc.query_selector("#user").unwrap().expect("user field");
    press_enter(&mut doc, user);

    assert!(
        navigation.navigations.lock().unwrap().is_empty(),
        "with no submit button, more than one blocking field must block submission"
    );
}

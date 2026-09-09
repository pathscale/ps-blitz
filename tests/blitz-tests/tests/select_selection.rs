//! A `<select>` can be focussed and its selection is what the form submits.
//!
//! `handle_click` had no arm for a select, so the walk fell through to the
//! no-match tail, which calls `clear_focus()`. Pressing a select therefore
//! actively unfocused the page, and since the keyboard handler is gated on
//! focus, the arrows could not drive a control the user had just pressed.
//!
//! Form submission had a standing TODO exactly where a select's serialization
//! belongs, so a select fell through to the generic tail and submitted its own
//! literal `value` attribute, which a select does not have.

use blitz_dom::DocumentConfig;
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_test_harness::Harness;
use blitz_traits::navigation::{NavigationOptions, NavigationProvider};
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::{Arc, Mutex};

const PICKER: &str = r#"<html><body style="margin:0">
    <select id="country" style="width:150px; height:30px">
        <option value="us">United States</option>
        <option value="fr" selected>France</option>
    </select>
</body></html>"#;

#[test]
fn clicking_a_select_focuses_it() {
    let mut harness = Harness::from_html(PICKER);
    let select = harness.node("#country");
    harness.click("#country");

    assert_eq!(
        harness.focused(),
        Some(select),
        "a press must focus the select; without it the keyboard handler, which is gated on focus, can never reach it"
    );
}

#[test]
fn clicking_a_select_does_not_clear_an_existing_focus() {
    let mut harness = Harness::from_html(
        r#"<html><body style="margin:0">
            <input id="name" type="text" style="width:100px; height:20px">
            <select id="country" style="width:150px; height:30px">
                <option value="us">United States</option>
            </select>
        </body></html>"#,
    );
    harness.click("#name");
    assert_eq!(harness.focused(), Some(harness.node("#name")));

    harness.click("#country");
    assert_eq!(
        harness.focused(),
        Some(harness.node("#country")),
        "the press must move focus to the select rather than clearing it"
    );
}

#[derive(Default)]
struct RecordingNavigation {
    urls: Mutex<Vec<String>>,
}

impl NavigationProvider for RecordingNavigation {
    fn navigate_to(&self, options: NavigationOptions) {
        self.urls
            .lock()
            .unwrap()
            .push(options.url.as_str().to_string());
    }
}

/// Submit the form and return the URL the navigation provider was handed.
fn submitted_url(html: &str) -> String {
    let navigation = Arc::new(RecordingNavigation::default());
    let mut doc = HtmlDocument::from_html(
        html,
        DocumentConfig {
            viewport: Some(Viewport::new(800, 600, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            navigation_provider: Some(navigation.clone()),
            base_url: Some("https://example.test/page".to_string()),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    let form = doc.query_selector("#form").unwrap().unwrap();
    let submitter = doc.query_selector("#go").unwrap().unwrap();
    doc.submit_form(form, submitter);

    let urls = navigation.urls.lock().unwrap();
    assert_eq!(urls.len(), 1, "expected exactly one navigation");
    urls[0].clone()
}

#[test]
fn a_form_submits_the_selected_options_value() {
    let url = submitted_url(
        r#"<html><body>
            <form id="form" action="/search" method="get">
                <select id="country" name="country">
                    <option value="us">United States</option>
                    <option value="fr" selected>France</option>
                </select>
                <button id="go" type="submit">Go</button>
            </form>
        </body></html>"#,
    );

    assert_eq!(
        url, "https://example.test/search?country=fr",
        "the entry must be the selected option's value, not the select's own (absent) value attribute"
    );
}

#[test]
fn an_option_with_no_value_attribute_submits_its_label() {
    let url = submitted_url(
        r#"<html><body>
            <form id="form" action="/search" method="get">
                <select id="size" name="size">
                    <option>Small</option>
                    <option selected>Large</option>
                </select>
                <button id="go" type="submit">Go</button>
            </form>
        </body></html>"#,
    );

    assert_eq!(url, "https://example.test/search?size=Large");
}

#[test]
fn a_disabled_option_is_never_submitted() {
    // `multiple` so that both options can carry `selected` at once: the point
    // of the case is that the disabled one is dropped while the other is kept,
    // which a single select could not express.
    let url = submitted_url(
        r#"<html><body>
            <form id="form" action="/search" method="get">
                <select id="country" name="country" multiple>
                    <option value="us" selected disabled>United States</option>
                    <option value="fr" selected>France</option>
                </select>
                <button id="go" type="submit">Go</button>
            </form>
        </body></html>"#,
    );

    assert_eq!(
        url, "https://example.test/search?country=fr",
        "a disabled option is skipped by the entry construction steps even when it is selected"
    );
}

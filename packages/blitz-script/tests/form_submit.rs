//! A page can handle its own form submission.
//!
//! Submitting used to be a default action nothing could see: pressing a submit
//! button called `submit_form` directly and no `submit` event was ever
//! dispatched, so a page's handler never ran and `preventDefault` had nothing
//! to prevent. Every framework form in every application therefore navigated
//! the browser to the form's action instead of running its own code -- which
//! for a single-page application means a full reload that throws away the step
//! it was on.
//!
//! Measured before the fix, on a page whose handler prevents the default: the
//! handler did not run and the document left for `/submitted.html?q=typed`.
//!
//! Both halves are asserted here, because either one alone passes for the
//! wrong reason. A handler that runs while the navigation happens anyway is
//! still broken, and a navigation that does not happen because the event was
//! never dispatched is the bug this replaces.

use blitz_dom::{Document, DocumentConfig};
use blitz_script::ScriptDocument;
use blitz_traits::events::DomEvent;
use blitz_traits::navigation::{NavigationOptions, NavigationProvider};
use keyboard_types::Modifiers;

/// Records what the shell was asked to navigate to, and navigates nowhere.
#[derive(Default)]
struct RecordedNavigation(std::sync::Mutex<Vec<String>>);

impl NavigationProvider for RecordedNavigation {
    fn navigate_to(&self, options: NavigationOptions) {
        if let Ok(mut destinations) = self.0.lock() {
            destinations.push(options.url.to_string());
        }
    }
}

fn page(script: &str) -> (ScriptDocument, std::sync::Arc<RecordedNavigation>) {
    let navigation = std::sync::Arc::new(RecordedNavigation::default());
    let document = ScriptDocument::from_html(
        &format!(
            r#"<html><body>
                 <form id="f" action="https://example.com/submitted">
                   <input id="q" name="q" value="typed">
                   <button id="go" type="submit">Go</button>
                 </form>
                 <p id="out">not yet</p>
                 <script>{script}</script>
               </body></html>"#
        ),
        DocumentConfig {
            base_url: Some("https://example.com/".to_owned()),
            navigation_provider: Some(navigation.clone()),
            ..Default::default()
        },
    );
    (document, navigation)
}

fn press_submit(document: &mut ScriptDocument) {
    let (button, click) = {
        let inner = document.inner();
        let button = inner
            .query_selector("#go")
            .unwrap()
            .expect("the fixture has a submit button");
        let click = inner
            .get_node(button)
            .unwrap()
            .synthetic_click_event(Modifiers::empty());
        (button, click)
    };
    document.dispatch_dom_event(DomEvent::new(button, click));
    document.poll(None);
}

fn text_of(document: &ScriptDocument, selector: &str) -> String {
    let inner = document.inner();
    let id = inner.query_selector(selector).unwrap().unwrap();
    inner.get_node(id).unwrap().text_content()
}

/// The event reaches the page, and preventing it stops the navigation.
#[test]
fn a_prevented_submission_does_not_navigate() {
    let (mut document, navigation) = page(
        r#"document.getElementById('f').addEventListener('submit', function (event) {
             event.preventDefault();
             document.getElementById('out').textContent = 'handled';
           });"#,
    );
    document.execute_scripts();
    press_submit(&mut document);

    assert_eq!(
        text_of(&document, "#out"),
        "handled",
        "the page's submit handler never ran"
    );
    assert!(
        navigation.0.lock().unwrap().is_empty(),
        "the form navigated even though the page prevented it: {:?}",
        navigation.0.lock().unwrap()
    );
}

/// And a submission nobody handles still submits.
///
/// The fix must not turn every form into a dead one. A page with no listener
/// gets the browser's behaviour, which is the navigation it always did.
#[test]
fn an_unhandled_submission_still_navigates() {
    let (mut document, navigation) = page("");
    document.execute_scripts();
    press_submit(&mut document);

    let destinations = navigation.0.lock().unwrap();
    assert_eq!(
        destinations.len(),
        1,
        "a form nobody handled should still submit: {destinations:?}"
    );
    assert!(
        destinations[0].contains("/submitted"),
        "submitted to the wrong place: {destinations:?}"
    );
}

/// A listener that does not prevent the default is not a refusal either.
#[test]
fn an_observed_submission_still_navigates() {
    let (mut document, navigation) = page(
        r#"document.getElementById('f').addEventListener('submit', function () {
             document.getElementById('out').textContent = 'seen';
           });"#,
    );
    document.execute_scripts();
    press_submit(&mut document);

    assert_eq!(text_of(&document, "#out"), "seen");
    assert_eq!(
        navigation.0.lock().unwrap().len(),
        1,
        "watching a submission is not cancelling it"
    );
}

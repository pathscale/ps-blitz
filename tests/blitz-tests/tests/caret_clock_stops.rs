//! A caret clock belongs to a focused input, and stops when the focus leaves.
//!
//! `AnimationPacing::Caret` asks for a frame every 500ms forever. That is
//! correct while an input is focused and blinking, and a leak otherwise: the
//! document never reaches `Idle`, so the window repaints twice a second for
//! the life of the process with nothing on screen changing.
//!
//! Measured in AgencyZero: 2.0fps with no input, no CSS animation, no canvas
//! and nothing focused (`focusedNode` was the document root). The default
//! animation diagnostic cannot see this, because `is_animating` deliberately
//! excludes the caret from its reasons, so `BLITZ_ANIMATION_DEBUG=1` prints
//! nothing at all while the clock runs.
//!
//! Reported as "a little blinking thing" with nothing logical near it.

use blitz_dom::{AnimationPacing, Document, DocumentConfig};
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

fn doc_with_input() -> HtmlDocument {
    let mut doc = HtmlDocument::from_html(
        r#"<html><body style="margin:0">
             <input id="field" value="hello">
             <div id="plain">not an input</div>
           </body></html>"#,
        DocumentConfig {
            viewport: Some(Viewport::new(200, 80, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    doc
}

/// The baseline: an untouched document asks for no frames at all.
#[test]
fn an_untouched_document_is_idle() {
    let doc = doc_with_input();
    assert_eq!(
        doc.animation_pacing(),
        AnimationPacing::Idle,
        "a document nobody has focused is already asking for frames"
    );
}

/// Focusing an input starts the clock, and moving focus to something that is
/// not an input has to stop it again.
#[test]
fn moving_focus_off_an_input_stops_the_caret_clock() {
    let mut doc = doc_with_input();
    let field = doc.query_selector("#field").unwrap().expect("#field");
    let plain = doc.query_selector("#plain").unwrap().expect("#plain");

    doc.set_focus_to(field);
    assert_eq!(
        doc.animation_pacing(),
        AnimationPacing::Caret,
        "focusing an input did not start a caret clock"
    );

    doc.set_focus_to(plain);
    assert_eq!(
        doc.animation_pacing(),
        AnimationPacing::Idle,
        "focus moved off the input but the 500ms caret clock kept running; \
         the document repaints twice a second forever with nothing changing"
    );
}

/// And removing the focused input entirely must not leave its clock behind.
///
/// This is the case a tab switch hits: the pane holding the focused field is
/// unmounted, and whatever `focus_node_id` still points at is gone.
#[test]
fn removing_a_focused_input_stops_the_caret_clock() {
    let mut doc = doc_with_input();
    let field = doc.query_selector("#field").unwrap().expect("#field");

    doc.set_focus_to(field);
    assert_eq!(doc.animation_pacing(), AnimationPacing::Caret);

    {
        let mut mutator = doc.mutate();
        mutator.remove_node(field);
    }
    doc.inner_mut().resolve(0.0);

    assert_eq!(
        doc.animation_pacing(),
        AnimationPacing::Idle,
        "the focused input was removed but its caret clock outlived it"
    );
}

use blitz_dom::{AnimationPacing, DocumentConfig};
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

const PAGE: &str = r#"<html><body style="margin:0;background:white"><input id="field" value="hello" style="margin:10px;width:160px;height:40px;color:black;background:white"></body></html>"#;

/// Focusing a text input has to put the document on the caret clock.
///
/// Font-independent, so it runs everywhere: it reads what the document asks its
/// shell for, not what came out of the rasteriser.
///
/// This file used to carry a second test that painted two frames half a blink
/// apart and asserted the buffers differed. It is gone rather than quarantined.
/// `assert_ne!` over a whole 200x80 buffer is satisfied by any difference
/// anywhere, so it never established that the thing which changed was the
/// caret, or where it was; and the phases it sampled, 0.25 and 0.75, hardcoded
/// a blink period the test never stated, so changing that period would have
/// moved it to comparing two same-phase frames. It had also been dead on Linux
/// for its whole life, passing on two identically blank renders.
///
/// What went with it: nothing else asserts the caret ever turns *off*.
/// `caret_respects_ancestor_clip` only ever renders the on phase. That is
/// cosmetic, and worth re-adding deliberately -- anchored to the caret's own
/// rect rather than to the whole frame -- if it ever matters.
#[test]
fn focusing_a_text_input_puts_the_document_on_the_caret_clock() {
    let mut doc = HtmlDocument::from_html(
        PAGE,
        DocumentConfig {
            viewport: Some(Viewport::new(200, 80, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    assert_ne!(
        doc.animation_pacing(),
        AnimationPacing::Caret,
        "an unfocused document must not be asking for caret-paced frames"
    );

    let field = doc.query_selector("#field").unwrap().unwrap();
    doc.set_focus_to(field);
    assert_eq!(
        doc.animation_pacing(),
        AnimationPacing::Caret,
        "focusing a text input has to put the document on the caret clock"
    );
}

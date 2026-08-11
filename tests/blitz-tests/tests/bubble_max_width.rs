//! A capped, shrink-to-fit flex item must stay inside its cap.
//!
//! Derived from a real transcript, not invented: an agent message bubble is a
//! `max-width: 88%` column item aligned with `align-self: flex-start`, and the
//! owner's screenshot shows one running off the right of the window with its
//! prose wrapped to the escaped width rather than to the bubble.
//!
//! The application already worked around this once and left the note in
//! `TranscriptPane.tsx`: `min-width: 0` and `overflow: hidden` were added to
//! stop long words escaping the bubble, and had to come back out because under
//! Blitz they collapsed the same `self-start` child to zero width and the
//! transcript rendered empty. So both states are wrong, which is the shape of
//! an engine bug rather than a styling mistake.
//!
//! The cap is not advisory. `max-width` is applied after the automatic minimum
//! size in CSS sizing, and the automatic minimum of a column flex item is on
//! its main axis, which is height. Nothing about a long word entitles the width
//! to exceed 88%.

use blitz_dom::DocumentConfig;
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

const PANE: f32 = 800.0;

fn transcript(body: &str) -> HtmlDocument {
    // The ancestor chain as the transcript actually builds it: a scroller, a
    // column of rows, and the bubble as a `self-start` item in that column.
    let html = format!(
        // `border-box` because Tailwind's preflight sets it on everything, and
        // without it the cap bounds the content box and every number here is
        // 32px of padding out.
        r#"<html><head><style>*{{box-sizing:border-box}}</style></head><body style="margin:0">
            <div id="scroller" style="width:{PANE}px; height:600px; overflow-y:auto;
                                      display:flex; flex-direction:column;">
              <div id="row" style="display:flex; flex-direction:column; min-width:0;">
                <div id="bubble" style="display:flex; flex-direction:column; gap:8px;
                                        align-self:flex-start; max-width:88%;
                                        padding:12px 16px; font-size:14px;">
                  <div id="body" style="display:flex; min-width:0; flex-direction:column;
                                        overflow-wrap:anywhere; word-break:break-word;">
                    {body}
                  </div>
                </div>
              </div>
            </div>
          </body></html>"#
    );
    let mut doc = HtmlDocument::from_html(
        &html,
        DocumentConfig {
            viewport: Some(Viewport::new(PANE as u32, 600, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    doc
}

fn width_of(doc: &HtmlDocument, selector: &str) -> f32 {
    let id = doc
        .query_selector(selector)
        .unwrap()
        .unwrap_or_else(|| panic!("no {selector}"));
    doc.get_node(id).unwrap().final_layout().size.width
}

/// The cap holds for ordinary prose. This is the control: if this fails the
/// fixture is wrong, not the engine.
#[test]
fn prose_wraps_inside_the_cap() {
    let doc = transcript(
        "<p>I am checking the running bundle identity, active profile path, and \
         both stores to determine where the data went and which process holds it.</p>",
    );
    let bubble = width_of(&doc, "#bubble");
    assert!(
        bubble <= PANE * 0.88 + 0.5,
        "bubble {bubble} exceeds the 88% cap ({})",
        PANE * 0.88
    );
}

/// The reported bug. One unbreakable run, which is what a base64 blob or a very
/// long path is, must not widen the bubble past its cap: `overflow-wrap:
/// anywhere` on the body is there precisely so it can break.
#[test]
fn one_unbreakable_run_does_not_widen_the_bubble() {
    let long = "x".repeat(2_000);
    let doc = transcript(&format!("<p>Here is the blob: {long}</p>"));
    let bubble = width_of(&doc, "#bubble");
    assert!(
        bubble <= PANE * 0.88 + 0.5,
        "bubble {bubble} exceeds the 88% cap ({}): an unbreakable run widened it",
        PANE * 0.88
    );
}

/// The transcript is not a fixed-width box. It is `flex-1` in a row beside the
/// project sidebar, so its width is resolved by flex rather than declared, and
/// the bubble's `max-width: 88%` is a percentage against that. A percentage
/// against a containing block that is indefinite while it is being measured
/// resolves to no cap at all, which would let the bubble take max-content: one
/// unwrapped line, exactly what the owner's screenshot shows for 515 characters
/// of ordinary prose with no token longer than 28 characters in it.
#[test]
fn the_cap_survives_a_pane_whose_width_comes_from_flex() {
    let html = format!(
        r#"<html><head><style>*{{box-sizing:border-box}}</style></head><body style="margin:0">
            <div style="display:flex; width:{PANE}px; height:600px;">
              <div id="sidebar" style="width:240px; flex:0 0 auto;"></div>
              <div id="scroller" style="flex:1 1 0%; min-width:0; overflow-y:auto;
                                        display:flex; flex-direction:column;">
                <div id="row" style="display:flex; flex-direction:column; min-width:0;">
                  <div id="bubble" style="display:flex; flex-direction:column; gap:8px;
                                          align-self:flex-start; max-width:88%;
                                          padding:12px 16px; font-size:14px;">
                    <div id="body" style="display:flex; min-width:0; flex-direction:column;
                                          overflow-wrap:anywhere; word-break:break-word;">
                      <p>I am checking the running bundle identity, the active profile path,
                      and both stores to determine whether data was deleted or the app
                      opened the wrong profile. The data still exists.</p>
                    </div>
                  </div>
                </div>
              </div>
            </div>
          </body></html>"#
    );
    let mut doc = HtmlDocument::from_html(
        &html,
        DocumentConfig {
            viewport: Some(Viewport::new(PANE as u32, 600, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);

    let pane = width_of(&doc, "#scroller");
    let bubble = width_of(&doc, "#bubble");
    assert!(pane > 0.0, "the pane did not resolve a width");
    assert!(
        bubble <= pane * 0.88 + 0.5,
        "bubble {bubble} exceeds 88% of the {pane}px pane ({})",
        pane * 0.88
    );
}

/// The other half, and the reason the workaround was reverted: a capped
/// `self-start` item with real content must not measure zero either.
#[test]
fn the_bubble_does_not_collapse_to_nothing() {
    let doc = transcript("<p>Reconnected</p>");
    let bubble = width_of(&doc, "#bubble");
    assert!(bubble > 1.0, "bubble collapsed to {bubble}");
}

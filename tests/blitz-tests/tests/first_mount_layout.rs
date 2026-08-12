//! A subtree mounted into a live document lays out against its real width.
//!
//! Reported as "1st load is fucked": an assistant message rendered as a column
//! of one or two words per line inside a full-width bubble, i.e. line-broken
//! at a fraction of the width of the box it was painted in. A container laid
//! out with a default taffy style behaves exactly like that — `Style::default()`
//! is `flex-direction: row`, so a column of paragraphs becomes a row of narrow
//! ones.
//!
//! The application mounts a project pane when the tab is first opened, so this
//! inserts markup into an already-resolved document rather than parsing it in.
//!
//!   cargo test --release -p blitz-tests --test first_mount_layout -- --nocapture

use blitz_dom::{Document as _, DocumentConfig};
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

const WIDTH: u32 = 1000;
const HEIGHT: u32 = 600;

/// A column of prose inside a flex chain, which is the transcript's shape.
const PANE: &str = r#"
  <div style="display:flex; flex-direction:column; flex:1; min-width:0;">
    <div class="bubble" style="display:flex; flex-direction:column; gap:8px; padding:12px;">
      <p id="prose">Perfect, proceed with backup here and restore there. I am monitoring
         the active PID and descriptor and will reattach to whichever replacement
         comes back, so nothing is lost between the two of them.</p>
    </div>
  </div>"#;

fn document(hidden: bool) -> HtmlDocument {
    let visibility = if hidden {
        "display:none"
    } else {
        "display:flex"
    };
    let html = format!(
        r#"<html><body style="margin:0">
             <div style="display:flex; flex-direction:column; width:{WIDTH}px; height:{HEIGHT}px;">
               <div id="pane" style="{visibility}; flex:1; min-width:0;"></div>
             </div>
           </body></html>"#
    );
    let mut doc = HtmlDocument::from_html(
        &html,
        DocumentConfig {
            viewport: Some(Viewport::new(WIDTH, HEIGHT, 1.0, ColorScheme::Dark)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    doc
}

fn mount(doc: &mut HtmlDocument, html: &str) {
    let inner = &mut *doc.inner_mut();
    let pane = inner.query_selector("#pane").unwrap().unwrap();
    let mut mutator = inner.mutate();
    mutator.set_inner_html(pane, html);
    drop(mutator);
    inner.resolve(0.0);
}

fn reveal(doc: &mut HtmlDocument) {
    let inner = &mut *doc.inner_mut();
    let pane = inner.query_selector("#pane").unwrap().unwrap();
    let mut mutator = inner.mutate();
    mutator.set_attribute(
        pane,
        blitz_dom::QualName {
            prefix: None,
            ns: blitz_dom::ns!(),
            local: "style".into(),
        },
        "display:flex; flex:1; min-width:0",
    );
    drop(mutator);
    inner.resolve(0.0);
}

fn prose_width(doc: &HtmlDocument) -> f32 {
    let id = doc.query_selector("#prose").unwrap().expect("#prose");
    doc.get_node(id).unwrap().final_layout().size.width
}

/// Mounted into a visible pane: the everyday first render of a tab.
#[test]
fn a_subtree_mounted_into_a_visible_pane_gets_the_full_width() {
    let mut doc = document(false);
    mount(&mut doc, PANE);

    let width = prose_width(&doc);
    assert!(
        width > 800.0,
        "prose laid out {width}px wide in a {WIDTH}px pane: its container is \
         laying out from a default taffy style, which is `flex-direction: row`, \
         so a column of text becomes a row of narrow ones"
    );
}

/// Mounted into a hidden pane and then revealed, which is what the application
/// does when a background tab is opened before it is switched to.
#[test]
fn a_subtree_mounted_while_hidden_gets_the_full_width_when_revealed() {
    let mut doc = document(true);
    mount(&mut doc, PANE);
    reveal(&mut doc);

    let width = prose_width(&doc);
    assert!(
        width > 800.0,
        "prose laid out {width}px wide after being mounted hidden and revealed"
    );
}

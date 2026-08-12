//! A hidden pane must not appear in the painted frame.
//!
//! The paint-list guard in `hidden_pane_hoisting` asserts on the data
//! structure; this asserts on the pixels, which is the thing the user actually
//! reported. A retained tab's raised children were painted over the tab in
//! front — one ghost chevron per open tab, and whole stale panels — because a
//! z-raised child is hoisted into an ancestor's stacking context and painted
//! from there, never passing the `display: none` check paint makes at the pane.
//!
//!   cargo test --release -p blitz-tests --test hidden_pane_paint -- --nocapture

use anyrender::render_to_buffer;
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_dom::{Document as _, DocumentConfig, QualName, ns};
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_paint::paint_scene;
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

const WIDTH: u32 = 400;
const HEIGHT: u32 = 200;
/// The two class lists App.tsx swaps between on a retained pane.
const SHOWN: &str = "shown";
const HIDDEN: &str = "hidden";

/// Distinctive, so a stray pixel names which pane it came from.
const GHOST_RED: [u8; 3] = [255, 0, 0];
const LIVE_GREEN: [u8; 3] = [0, 128, 0];

fn attr_name(local: &str) -> QualName {
    QualName {
        prefix: None,
        ns: ns!(),
        local: local.into(),
    }
}

/// Two panes stacked in the same place, each with a raised child that sticks
/// out at a known point. That is the application's shape: retained tabs in one
/// container, one visible, each holding `absolute … z-20` chrome.
fn overlapping_panes() -> HtmlDocument {
    let html = format!(
        r#"<html><head><style>
             .hidden {{ display: none }}
             .shown {{ display: flex; flex: 1; min-width: 0 }}
             .pane {{ position: relative; width: 400px; height: 200px }}
             .raised {{ position: absolute; top: 80px;
                        width: 60px; height: 40px; z-index: 20 }}
           </style></head>
           <body style="margin:0">
             <div style="display:flex; width:400px; height:200px;">
               <div id="tab0" class="{SHOWN}">
                 <div class="pane">
                   <div class="raised" style="left:40px; background:#ff0000"></div>
                 </div>
               </div>
               <div id="tab1" class="{HIDDEN}">
                 <div class="pane">
                   <div class="raised" style="left:240px; background:#008000"></div>
                 </div>
               </div>
             </div>
           </body></html>"#
    );
    let mut doc = HtmlDocument::from_html(
        &html,
        DocumentConfig {
            viewport: Some(Viewport::new(WIDTH, HEIGHT, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    doc
}

fn switch_to(doc: &mut HtmlDocument, shown: &str, hidden: &str) {
    let inner = &mut *doc.inner_mut();
    let mut mutator = inner.mutate();
    let shown_id = mutator.doc.query_selector(shown).unwrap().unwrap();
    let hidden_id = mutator.doc.query_selector(hidden).unwrap().unwrap();
    mutator.set_attribute(hidden_id, attr_name("class"), HIDDEN);
    mutator.set_attribute(shown_id, attr_name("class"), SHOWN);
    drop(mutator);
    inner.resolve(0.0);
}

/// The colour at a point, as painted.
fn pixel_at(doc: &mut HtmlDocument, x: u32, y: u32) -> [u8; 3] {
    let buffer = render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| paint_scene(scene, doc, 1.0, WIDTH, HEIGHT, 0, 0),
        WIDTH,
        HEIGHT,
    );
    let idx = ((y * WIDTH + x) * 4) as usize;
    [buffer[idx], buffer[idx + 1], buffer[idx + 2]]
}

/// Inside tab0's raised child, and clear of tab1's. The two must not overlap:
/// with both at the same point the revealed pane simply paints over the ghost,
/// and the probe reads correct while the bug is fully present. That is what
/// the first version of this test did.
const PROBE: (u32, u32) = (70, 100);
/// Inside tab1's raised child, to confirm the revealed pane paints at all.
const LIVE_PROBE: (u32, u32) = (270, 100);

#[test]
fn a_hidden_panes_raised_child_does_not_paint() {
    let mut doc = overlapping_panes();

    assert_eq!(
        pixel_at(&mut doc, PROBE.0, PROBE.1),
        GHOST_RED,
        "fixture: the visible pane's raised child should paint"
    );

    switch_to(&mut doc, "#tab1", "#tab0");

    let painted = pixel_at(&mut doc, PROBE.0, PROBE.1);
    assert_ne!(
        painted, GHOST_RED,
        "the hidden pane's raised child is still being painted over the tab in \
         front: a z-raised child is hoisted into an ancestor's stacking context, \
         so it never passes the display:none check paint makes at the pane"
    );
    assert_eq!(
        pixel_at(&mut doc, LIVE_PROBE.0, LIVE_PROBE.1),
        LIVE_GREEN,
        "the revealed pane's own raised child should paint"
    );
}

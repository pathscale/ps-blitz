//! Nothing in the transcript may paint outside the transcript.
//!
//! The owner's screenshot shows an agent message whose prose runs off the right
//! of the window, wrapped to a width wider than its own bubble. 515 characters
//! of ordinary text, no token longer than 28 characters, so nothing about the
//! content forces it.
//!
//! Two hand-written fixtures failed to reproduce it and both passed, which
//! showed only that the fixtures were wrong. A bubble's width comes from a
//! chain of flex items, percentage caps and `min-width` rules that nobody
//! reconstructs from memory. So this test uses the real thing: the markup is
//! rendered by the application's own components (see
//! `TranscriptMarkup.test.tsx`, which writes `fixtures/transcript.html`), from
//! a thread taken out of the owner's store with every letter and digit replaced
//! and every word length, line break and message length kept, and the
//! stylesheet is the one the application ships.
//!
//! Regenerate the fixture with:
//!   cd agencyzero/apps/gui/frontend
//!   npx vitest run src/features/project/TranscriptMarkup.test.tsx

use anyrender::render_to_buffer;
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_dom::DocumentConfig;
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_paint::paint_scene;
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

/// The real window the screenshot was taken in.
const WIDTH: u32 = 1344;
const HEIGHT: u32 = 900;

fn document() -> HtmlDocument {
    let markup = include_str!("../fixtures/transcript.html");
    let css = include_str!("../fixtures/app.css");
    // The shell the pane lives in: a column that owns the height, with the
    // transcript as the flex child that scrolls. `min-height: 0` because
    // without it a flex child refuses to shrink below its content and the
    // scroller never establishes a scrollport, which would be a different bug
    // than the one under test.
    let html = format!(
        r#"<html><head><style>{css}</style></head>
           <body class="bg-base-100" style="margin:0">
             <div style="display:flex; flex-direction:column; width:{WIDTH}px; height:{HEIGHT}px;">
               {markup}
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

/// The same transcript, mounted into a document that has already been laid out
/// once, which is what the reveal path does.
///
/// This is the case that matters. A first paint gets the layout right (the
/// tests above pass), and the owner sees the spill after scrolling up, which is
/// when `revealEarlier` mounts rows into a pane that already has a resolved
/// width. So the question is not "does this markup lay out", it is "does it lay
/// out the same when it arrives second".
fn document_after_mount(incremental: bool) -> HtmlDocument {
    let css = include_str!("../fixtures/app.css");
    let markup = include_str!("../fixtures/transcript.html");
    let html = format!(
        r#"<html><head><style>{css}</style></head>
           <body class="bg-base-100" style="margin:0">
             <div id="shell" style="display:flex; flex-direction:column; width:{WIDTH}px; height:{HEIGHT}px;"></div>
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
    doc.set_incremental_layout(incremental);
    doc.resolve(0.0);

    let shell = doc.query_selector("#shell").unwrap().expect("no shell");
    doc.mutate().set_inner_html(shell, markup);
    doc.resolve(0.0);
    doc
}

fn widest_overhang(doc: &HtmlDocument) -> Option<(f32, f64, f64)> {
    let pane_id = doc
        .query_selector("[aria-label='Conversation']")
        .unwrap()
        .expect("no transcript");
    let pane = doc.get_node(pane_id).unwrap().final_layout();
    let right_edge = pane.location.x + pane.size.width;

    let mut worst: Option<(f32, f64, f64)> = None;
    for (id, _) in doc.tree().iter() {
        let Some(node) = doc.get_node(id) else {
            continue;
        };
        if node.element_data().is_none() {
            continue;
        }
        let Some(rect) = doc.get_client_bounding_rect(id) else {
            continue;
        };
        if rect.width <= 0.0 || rect.height <= 0.0 {
            continue;
        }
        let overhang = (rect.x + rect.width) as f32 - right_edge;
        if overhang > 0.5 && worst.is_none_or(|(w, _, _)| overhang > w) {
            worst = Some((overhang, rect.x, rect.width));
        }
    }
    worst
}

#[test]
fn rows_mounted_into_a_laid_out_pane_stay_inside_it() {
    let doc = document_after_mount(true);
    assert_eq!(
        widest_overhang(&doc),
        None,
        "a box hangs past the transcript after mounting into a resolved pane"
    );
}

/// The control. If this one passes while the incremental one fails, the layout
/// is right and the invalidation is wrong, which is a different fix in a
/// different crate.
#[test]
fn the_same_mount_without_incremental_layout() {
    let doc = document_after_mount(false);
    assert_eq!(
        widest_overhang(&doc),
        None,
        "a box hangs past the transcript with incremental layout off, so this is not invalidation"
    );
}

/// The fixture has to actually be a transcript, or every assertion below is
/// vacuously true. This is the guard against a silently empty fixture.
#[test]
fn the_fixture_is_a_populated_transcript() {
    let doc = document();
    let pane = doc
        .query_selector("[aria-label='Conversation']")
        .unwrap()
        .expect("no transcript in the fixture");
    let layout = doc.get_node(pane).unwrap().final_layout();
    assert!(
        layout.size.width > 1_000.0 && layout.size.height > 100.0,
        "transcript laid out as {}x{}",
        layout.size.width,
        layout.size.height
    );
}

/// No box may extend past the pane that contains it. This catches a bubble that
/// escapes its cap.
#[test]
fn no_box_extends_past_the_transcript() {
    let doc = document();
    let pane_id = doc
        .query_selector("[aria-label='Conversation']")
        .unwrap()
        .expect("no transcript in the fixture");
    let pane = doc.get_node(pane_id).unwrap().final_layout();
    let right_edge = pane.location.x + pane.size.width;

    let mut worst: Option<(f32, f32, f32)> = None;
    for (id, _) in doc.tree().iter() {
        let Some(node) = doc.get_node(id) else {
            continue;
        };
        if node.element_data().is_none() {
            continue;
        }
        let Some(rect) = doc.get_client_bounding_rect(id) else {
            continue;
        };
        if rect.width <= 0.0 || rect.height <= 0.0 {
            continue;
        }
        let overhang = (rect.x + rect.width) as f32 - right_edge;
        if overhang > 0.5 && worst.is_none_or(|(w, _, _)| overhang > w) {
            worst = Some((overhang, rect.x as f32, rect.width as f32));
        }
    }

    assert!(
        worst.is_none(),
        "a box hangs {:?}px past the transcript's right edge at {right_edge}",
        worst
    );
}

/// The screenshot's actual symptom: ink to the right of where the transcript
/// ends. A box can stay inside its parent and still paint an unwrapped line
/// across it, so this checks the pixels rather than the boxes.
#[test]
fn nothing_paints_past_the_transcript() {
    let mut doc = document();
    let pane_id = doc
        .query_selector("[aria-label='Conversation']")
        .unwrap()
        .expect("no transcript in the fixture");
    let pane = doc.get_node(pane_id).unwrap().final_layout();
    let right_edge = (pane.location.x + pane.size.width).round() as u32;
    assert!(right_edge <= WIDTH);

    let buffer = render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| paint_scene(scene, &mut doc, 1.0, WIDTH, HEIGHT, 0, 0),
        WIDTH,
        HEIGHT,
    );

    // The background the transcript paints on, sampled rather than assumed: the
    // theme owns the colour and hard-coding one here would make this test a
    // test of the palette.
    let sample = |x: u32, y: u32| -> [u8; 3] {
        let index = ((y * WIDTH + x) * 4) as usize;
        [buffer[index], buffer[index + 1], buffer[index + 2]]
    };
    let background = sample(WIDTH - 2, 4);

    let mut ink = Vec::new();
    for y in 0..HEIGHT {
        for x in right_edge.saturating_sub(1)..WIDTH {
            let pixel = sample(x, y);
            let differs = (0..3).any(|c| pixel[c].abs_diff(background[c]) > 12);
            if differs {
                ink.push((x, y));
            }
        }
    }

    assert!(
        ink.len() < 32,
        "{} pixels painted right of the transcript edge at {right_edge}, first at {:?}",
        ink.len(),
        &ink[..ink.len().min(6)]
    );
}

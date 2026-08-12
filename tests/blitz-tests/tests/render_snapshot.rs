//! Render a document to a PNG so it can be looked at.
//!
//! Every other test here asserts on a number, which is right for a guard and
//! useless for "does this look wrong". Reasoning about pixels from a data
//! structure is how a ghosted tab got argued about instead of seen; this is the
//! cheapest way to put an actual frame in front of whoever is asking.
//!
//! Ignored by default: it writes files, and it is a tool rather than a check.
//!
//!   cargo test --release -p blitz-tests --test render_snapshot -- --ignored --nocapture
//!
//! `BLITZ_SNAPSHOT_DIR` chooses where the PNGs land (default `target/snapshots`).
//! `BLITZ_SNAPSHOT_HTML` renders a file of your own instead of the fixtures.

use anyrender::render_to_buffer;
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_dom::{Document as _, DocumentConfig, QualName, ns};
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_paint::paint_scene;
use blitz_traits::shell::{ColorScheme, Viewport};
use std::path::PathBuf;
use std::sync::Arc;

const WIDTH: u32 = 1344;
const HEIGHT: u32 = 900;

fn attr_name(local: &str) -> QualName {
    QualName {
        prefix: None,
        ns: ns!(),
        local: local.into(),
    }
}

fn snapshot_dir() -> PathBuf {
    let dir = std::env::var("BLITZ_SNAPSHOT_DIR").unwrap_or_else(|_| "target/snapshots".into());
    let dir = PathBuf::from(dir);
    std::fs::create_dir_all(&dir).expect("snapshot directory");
    dir
}

fn document(html: &str) -> HtmlDocument {
    let mut doc = HtmlDocument::from_html(
        html,
        DocumentConfig {
            viewport: Some(Viewport::new(WIDTH, HEIGHT, 1.0, ColorScheme::Dark)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    doc
}

/// Paint the document and write it out. Returns the path, printed so the
/// caller can open it.
fn write_png(doc: &mut HtmlDocument, name: &str) -> PathBuf {
    let buffer = render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| paint_scene(scene, doc, 1.0, WIDTH, HEIGHT, 0, 0),
        WIDTH,
        HEIGHT,
    );
    let path = snapshot_dir().join(format!("{name}.png"));
    image::save_buffer(
        &path,
        &buffer,
        WIDTH,
        HEIGHT,
        image::ExtendedColorType::Rgba8,
    )
    .expect("write png");
    println!("  wrote {}", path.display());
    path
}

/// The application's shape: retained tabs in one container, one visible.
fn tabbed_app() -> String {
    let css = include_str!("../fixtures/app.css");
    let markup = include_str!("../fixtures/transcript.html");
    let pane = |i: usize, class: &str| {
        format!(
            r#"<div id="tab{i}" class="{class}"><div style="display:flex; flex-direction:column; flex:1; min-width:0;">{markup}</div></div>"#
        )
    };
    format!(
        r#"<html><head><style>{css}</style></head>
           <body class="bg-base-100" style="margin:0">
             <div style="display:flex; flex-direction:column; width:{WIDTH}px; height:{HEIGHT}px;">{}{}</div>
           </body></html>"#,
        pane(0, "flex min-h-0 min-w-0 flex-1"),
        pane(1, "hidden")
    )
}

fn switch_to(doc: &mut HtmlDocument, shown: &str, hidden: &str) {
    let inner = &mut *doc.inner_mut();
    let mut mutator = inner.mutate();
    let shown_id = mutator.doc.query_selector(shown).unwrap().unwrap();
    let hidden_id = mutator.doc.query_selector(hidden).unwrap().unwrap();
    mutator.set_attribute(hidden_id, attr_name("class"), "hidden");
    mutator.set_attribute(shown_id, attr_name("class"), "flex min-h-0 min-w-0 flex-1");
    drop(mutator);
    inner.resolve(0.0);
}

/// Before and after a tab switch, so a ghost is visible rather than argued.
#[test]
#[ignore = "writes PNGs; run explicitly"]
fn snapshot_a_tab_switch() {
    let mut doc = document(&tabbed_app());
    println!("\n== tab switch ==");
    write_png(&mut doc, "tab-before-switch");
    switch_to(&mut doc, "#tab1", "#tab0");
    write_png(&mut doc, "tab-after-switch");
    println!();
}

/// Whatever `BLITZ_SNAPSHOT_HTML` points at.
#[test]
#[ignore = "writes PNGs; run explicitly"]
fn snapshot_a_file() {
    let Ok(path) = std::env::var("BLITZ_SNAPSHOT_HTML") else {
        println!("\n  set BLITZ_SNAPSHOT_HTML=<file> to render your own markup\n");
        return;
    };
    let html = std::fs::read_to_string(&path).expect("read BLITZ_SNAPSHOT_HTML");
    let mut doc = document(&html);
    println!("\n== {path} ==");
    write_png(&mut doc, "snapshot");
    println!();
}

//! End-to-end probe for Solid's compiled DOM and delegated event paths.

use std::path::PathBuf;

use anyrender::render_to_buffer;
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_dom::{Document, DocumentConfig};
use blitz_paint::paint_scene;
use blitz_script::ScriptDocument;
use blitz_traits::events::DomEvent;
use blitz_traits::shell::{ColorScheme, Viewport};
use keyboard_types::Modifiers;
use url::Url;

const WIDTH: u32 = 640;
const HEIGHT: u32 = 480;

fn probe_index() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/solid-probe/index.html")
}

fn load_probe() -> ScriptDocument {
    let index_path = probe_index().canonicalize().unwrap_or_else(|_| {
        panic!("Solid probe is not built; run the Rsbuild command documented in examples/solid")
    });
    let html = std::fs::read_to_string(&index_path).unwrap();
    let base_url = Url::from_file_path(&index_path).unwrap();
    let mut doc = ScriptDocument::from_html(
        &html,
        DocumentConfig {
            base_url: Some(base_url.to_string()),
            viewport: Some(Viewport::new(WIDTH, HEIGHT, 1.0, ColorScheme::Light)),
            ..Default::default()
        },
    );
    doc.execute_scripts();
    doc.inner_mut().resolve(0.0);
    doc
}

fn query(doc: &ScriptDocument, selector: &str) -> Option<usize> {
    doc.inner().query_selector(selector).unwrap()
}

fn query_all(doc: &ScriptDocument, selector: &str) -> Vec<usize> {
    doc.inner().query_selector_all(selector).unwrap().to_vec()
}

fn text(doc: &ScriptDocument, selector: &str) -> String {
    let node_id = query(doc, selector).unwrap_or_else(|| panic!("no node matching {selector}"));
    doc.inner().get_node(node_id).unwrap().text_content()
}

fn click(doc: &mut ScriptDocument, selector: &str) {
    doc.inner_mut().resolve(0.0);
    let event = {
        let inner = doc.inner();
        let node_id = inner
            .query_selector(selector)
            .unwrap()
            .unwrap_or_else(|| panic!("no node matching {selector}"));
        DomEvent::new(
            node_id,
            inner
                .get_node(node_id)
                .unwrap()
                .synthetic_click_event(Modifiers::empty()),
        )
    };
    doc.dispatch_dom_event(event);
}

fn render_png(doc: &mut ScriptDocument) -> PathBuf {
    doc.inner_mut().resolve(0.0);
    let mut inner = doc.inner_mut();
    let buffer = render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| paint_scene(scene, &mut inner, 1.0, WIDTH, HEIGHT, 0, 0),
        WIDTH,
        HEIGHT,
    );
    drop(inner);

    let path = probe_index().with_file_name("solid-probe.png");
    image::save_buffer_with_format(
        &path,
        &buffer,
        WIDTH,
        HEIGHT,
        image::ColorType::Rgba8,
        image::ImageFormat::Png,
    )
    .unwrap();
    path
}

#[test]
fn solid_reactivity_and_delegated_events_drive_the_dom() {
    let mut doc = load_probe();

    assert_eq!(text(&doc, "#count"), "0");
    assert_eq!(text(&doc, "#effect"), "effect:0");
    assert_eq!(
        query_all(&doc, "#items > li")
            .into_iter()
            .map(|id| doc.inner().get_node(id).unwrap().text_content())
            .collect::<Vec<_>>(),
        ["a", "b"]
    );
    assert!(query(&doc, "#over").is_none());

    click(&mut doc, "#increment");
    assert_eq!(text(&doc, "#count"), "1");
    assert_eq!(text(&doc, "#effect"), "effect:1");

    click(&mut doc, "#increment");
    click(&mut doc, "#increment");
    assert_eq!(text(&doc, "#count"), "3");
    assert_eq!(text(&doc, "#over"), "over");

    click(&mut doc, "#add");
    assert_eq!(
        query_all(&doc, "#items > li")
            .into_iter()
            .map(|id| doc.inner().get_node(id).unwrap().text_content())
            .collect::<Vec<_>>(),
        ["a", "b", "c"]
    );

    let png = render_png(&mut doc);
    assert!(std::fs::metadata(png).unwrap().len() > 0);
}

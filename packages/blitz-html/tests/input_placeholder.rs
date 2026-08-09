//! Placeholder rendering for text form controls.

use anyrender::render_to_buffer;
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_dom::DocumentConfig;
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_paint::paint_scene;
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

fn paints_non_white_pixels(doc: &mut HtmlDocument, width: u32, height: u32) -> bool {
    let buffer = render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| paint_scene(scene, doc, 1.0, width, height, 0, 0),
        width,
        height,
    );
    buffer
        .chunks_exact(4)
        .any(|pixel| pixel[0] < 245 || pixel[1] < 245 || pixel[2] < 245)
}

#[test]
fn empty_input_paints_its_placeholder_without_changing_its_value() {
    let mut doc = HtmlDocument::from_html(
        r#"<html><body style="margin:0; background:white;">
            <input id="search" placeholder="Search projects and items"
                   style="display:block; width:240px; height:40px; border:0;
                          padding:8px; background:white; color:black; font-size:16px;" />
        </body></html>"#,
        DocumentConfig {
            viewport: Some(Viewport::new(260, 60, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);

    let input_id = doc.query_selector("#search").unwrap().unwrap();
    let input = doc.get_node(input_id).unwrap().element_data().unwrap();
    let data = input.text_input_data().expect("text input data");
    assert_eq!(
        data.editor.text(),
        "",
        "placeholder must not become the value"
    );
    let placeholder = data.placeholder_editor.as_ref().unwrap();
    assert_eq!(placeholder.text(), "Search projects and items");
    let placeholder_layout = placeholder.try_layout().expect("placeholder layout");
    assert!(placeholder_layout.full_width() > 0.0);
    assert!(doc.get_node(input_id).unwrap().final_layout.size.width > 0.0);
    assert!(doc.get_node(input_id).unwrap().final_layout.size.height > 0.0);

    assert!(
        paints_non_white_pixels(&mut doc, 260, 60),
        "placeholder must produce visible text pixels"
    );
}

#[test]
fn populated_input_paints_its_value_with_computed_font_styles() {
    let mut doc = HtmlDocument::from_html(
        r#"<html><body style="margin:0; background:white;">
            <input id="search" value="Existing search" placeholder="Search projects"
                   style="display:block; width:240px; height:40px; border:0;
                          padding:8px; background:white; color:black; font:700 17px system-ui;" />
        </body></html>"#,
        DocumentConfig {
            viewport: Some(Viewport::new(260, 60, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);

    let input_id = doc.query_selector("#search").unwrap().unwrap();
    let input = doc.get_node(input_id).unwrap().element_data().unwrap();
    let data = input.text_input_data().expect("text input data");
    assert_eq!(data.editor.text(), "Existing search");
    assert!(data.editor.try_layout().unwrap().full_width() > 0.0);
    assert!(
        paints_non_white_pixels(&mut doc, 260, 60),
        "input value must produce visible text pixels"
    );
}

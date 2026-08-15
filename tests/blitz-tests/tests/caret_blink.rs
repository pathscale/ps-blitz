use anyrender::render_to_buffer;
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_dom::{AnimationPacing, DocumentConfig};
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_paint::paint_scene_at_time;
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

fn frame(doc: &mut HtmlDocument, time: f64) -> Vec<u8> {
    render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| paint_scene_at_time(scene, doc, 1.0, 200, 80, 0, 0, time),
        200,
        80,
    )
}

#[test]
fn a_focused_text_input_keeps_a_clock_and_blinks_its_caret() {
    let mut doc = HtmlDocument::from_html(
        r#"<html><body style="margin:0;background:white"><input id="field" value="hello" style="margin:10px;width:160px;height:40px;color:black;background:white"></body></html>"#,
        DocumentConfig {
            viewport: Some(Viewport::new(200, 80, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    let field = doc.query_selector("#field").unwrap().unwrap();
    doc.set_focus_to(field);
    assert_eq!(doc.animation_pacing(), AnimationPacing::Caret);

    let visible = frame(&mut doc, 0.25);
    let hidden = frame(&mut doc, 0.75);
    assert_ne!(
        visible, hidden,
        "the focused caret stayed continuously visible"
    );
}

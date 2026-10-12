use blitz_dom::DocumentConfig;
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

const HTML: &str = r#"<!doctype html><html><head><meta charset="utf-8">
<style>body{margin:0}#box{width:50vw;height:25vh}
#box::before{content:"";display:block;width:10vw;height:10vh}</style>
</head><body><div id="box"></div></body></html>"#;

fn make_doc(w: u32, h: u32) -> HtmlDocument {
    HtmlDocument::from_html(
        HTML,
        DocumentConfig {
            viewport: Some(Viewport::new(w, h, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    )
}

fn sizes(doc: &HtmlDocument) -> (f32, f32, f32, f32) {
    let id = doc.query_selector("#box").unwrap().unwrap();
    let node = doc.get_node(id).unwrap();
    let size = node.final_layout().size;
    let pseudo = doc
        .get_node(node.before().unwrap())
        .unwrap()
        .final_layout()
        .size;
    (size.width, size.height, pseudo.width, pseudo.height)
}

#[test]
fn resize_with_unstyled_head_descendant() {
    let mut doc = make_doc(800, 600);
    doc.resolve(0.0);
    let meta = doc.query_selector("meta").unwrap().unwrap();
    let meta_node = doc.get_node(meta).unwrap();
    assert!(meta_node.stylo_element_data_opt().unwrap().get().is_some());
    assert!(meta_node.primary_styles().is_none());
    assert_eq!(sizes(&doc), (400.0, 150.0, 80.0, 60.0));

    doc.viewport_mut().window_size = (1000, 800);
    doc.resolve(0.0);
    assert_eq!(sizes(&doc), (500.0, 200.0, 100.0, 80.0));
}

#[test]
fn resize_before_first_style() {
    let mut doc = make_doc(0, 0);
    doc.viewport_mut().window_size = (1000, 800);
    doc.resolve(0.0);
    assert_eq!(sizes(&doc), (500.0, 200.0, 100.0, 80.0));
}

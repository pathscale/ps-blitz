use blitz_dom::DocumentConfig;
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

fn document(style: &str, scale: f32) -> HtmlDocument {
    let mut doc = HtmlDocument::from_html(
        &format!(
            r#"<style>body {{ margin:0 }}
        #parent {{ position:absolute; left:100px; top:80px; width:300px; height:200px;
                   transform-origin:0 0; {style} }}
        #child {{ position:absolute; left:20px; top:30px; width:40px; height:20px }}
        </style><div id="parent"><div id="child"></div></div>"#
        ),
        DocumentConfig {
            viewport: Some(Viewport::new(800, 600, scale, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    doc
}

fn rect(doc: &HtmlDocument) -> [f64; 4] {
    let node = doc.query_selector("#child").unwrap().unwrap();
    let rect = doc.get_client_bounding_rect(node).unwrap();
    [rect.x, rect.y, rect.width, rect.height]
}

fn close(actual: [f64; 4], expected: [f64; 4]) {
    for (a, b) in actual.into_iter().zip(expected) {
        assert!((a - b).abs() < 0.1, "{actual:?}, expected {expected:?}");
    }
}

#[test]
fn scaled_translated_ancestor_changes_client_size_and_position() {
    close(
        rect(&document("transform:translate(10px,15px) scale(2)", 1.0)),
        [150.0, 155.0, 80.0, 40.0],
    );
}

#[test]
fn device_scale_does_not_change_css_client_coordinates() {
    close(
        rect(&document("transform:translate(10px,15px) scale(2)", 2.0)),
        [150.0, 155.0, 80.0, 40.0],
    );
}

#[test]
fn rotation_returns_the_axis_aligned_painted_bounds() {
    close(
        rect(&document("transform:rotate(90deg)", 1.0)),
        [50.0, 100.0, 20.0, 40.0],
    );
}

#[test]
fn nested_inverse_rotations_do_not_accumulate_bounding_box_error() {
    let mut doc = document("transform:rotate(45deg)", 1.0);
    let child = doc.query_selector("#child").unwrap().unwrap();
    doc.mutate().set_attribute(
        child,
        markup5ever::QualName::new(None, markup5ever::ns!(), "style".into()),
        "transform-origin:0 0;transform:rotate(-45deg)",
    );
    doc.resolve(0.0);
    let actual = rect(&doc);
    assert!((actual[2] - 40.0).abs() < 0.1, "{actual:?}");
    assert!((actual[3] - 20.0).abs() < 0.1, "{actual:?}");
}

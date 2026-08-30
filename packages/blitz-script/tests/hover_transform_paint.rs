//! A settled hover transform must not permanently change neutral paint.

#![cfg(feature = "debug-control")]

use anyrender::render_to_buffer;
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_dom::{Document as _, DocumentConfig};
use blitz_paint::paint_scene;
use blitz_script::ScriptDocument;
use blitz_traits::{
    events::{BlitzPointerEvent, MouseEventButtons},
    shell::{ColorScheme, Viewport},
};

const WIDTH: u32 = 190;
const HEIGHT: u32 = 190;

const HTML: &str = r#"<!doctype html>
<html>
<style>
  html, body { margin: 0; width: 190px; height: 190px; background: #0b1012; }
  #flower { position: relative; width: 190px; height: 190px; }
  #picker { position: absolute; left: 2.5px; top: 2.5px; width: 185px; height: 185px; }
  .dot {
    position: absolute;
    z-index: 1;
  }
  #one { left: 50%; top: 50%; margin-left: 47.631px; margin-top: -27.5px; }
  #two { left: 50%; top: 50%; margin-left: 34.641px; margin-top: -20px; }
  .frame {
    position: relative;
    width: 32px;
    height: 32px;
    margin: -16px 0 0 -16px;
  }
  .motion {
    position: relative;
    width: 100%;
    height: 100%;
    transform: translate(0, 0) scale(1);
    transform-origin: center;
    transition: transform 100ms ease-out;
  }
  .petal {
    position: relative;
    z-index: 10;
    width: 100%;
    height: 100%;
    border: 1px solid #aab0b4;
    border-radius: 9999px;
    box-shadow: 0 2px 8px rgb(0 0 0 / 25%);
  }
  #one .petal { background: #604810; }
  #two .petal { background: #8d491b; }
  .highlight {
    position: absolute;
    z-index: 11;
    inset: 0;
    border: 2px solid #604810;
    border-radius: 9999px;
    opacity: 0;
  }
  .dot:hover .motion { transform: translate(0, 0) scale(1.1); }
  .dot:hover .highlight { opacity: .75; }
</style>
<body><div id="flower"><div id="picker">
  <div id="one" class="dot"><div class="frame"><div class="motion"><div class="petal"></div><span id="highlight" class="highlight"></span></div></div></div>
  <div id="two" class="dot"><div class="frame"><div class="motion"><div class="petal"></div></div></div></div>
</div></div></body>
</html>"#;

fn resolve(document: &mut ScriptDocument, animation_time: f64) {
    document.inner_mut().resolve(animation_time);
}

fn render(document: &mut ScriptDocument) -> Vec<u8> {
    let mut inner = document.inner_mut();
    render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| paint_scene(scene, &mut inner, 1.0, WIDTH, HEIGHT, 0, 0),
        WIDTH,
        HEIGHT,
    )
}

fn paint_order(document: &ScriptDocument) -> Vec<(u64, Vec<(u64, i32)>)> {
    document
        .inner()
        .tree()
        .iter()
        .filter_map(|(node_id, node)| {
            node.stacking_context.as_ref().map(|context| {
                (
                    node_id.as_u64(),
                    context
                        .children
                        .iter()
                        .map(|child| (child.node_id.as_u64(), child.z_index))
                        .collect(),
                )
            })
        })
        .collect()
}

#[test]
fn a_hovered_transform_returns_to_the_original_neutral_pixels() {
    let mut document = ScriptDocument::from_html(HTML, DocumentConfig::default());
    document
        .inner_mut()
        .set_viewport(Viewport::new(WIDTH, HEIGHT, 1.0, ColorScheme::Dark));
    resolve(&mut document, 0.0);

    let (petal, motion, highlight, root) = {
        let inner = document.inner();
        (
            inner.query_selector("#one").unwrap().unwrap(),
            inner.query_selector("#one .motion").unwrap().unwrap(),
            inner.query_selector("#highlight").unwrap().unwrap(),
            inner.query_selector("html").unwrap().unwrap(),
        )
    };
    let before_transform = *document.inner().get_node(motion).unwrap().transform();
    let before_opacity = document
        .inner()
        .get_node(highlight)
        .unwrap()
        .primary_styles()
        .unwrap()
        .clone_opacity();
    let before_paint_order = paint_order(&document);
    let initial = render(&mut document);
    resolve(&mut document, 0.01);
    let before = render(&mut document);
    assert_eq!(
        before, initial,
        "an idle second resolve changed the initial neutral pixels"
    );

    document.handle_pointer_move_to_node(
        BlitzPointerEvent::at(126.0, 64.0, MouseEventButtons::empty()),
        petal,
    );
    resolve(&mut document, 0.02);
    resolve(&mut document, 0.15);
    document.handle_pointer_move_to_node(
        BlitzPointerEvent::at(0.0, 0.0, MouseEventButtons::empty()),
        root,
    );
    resolve(&mut document, 0.16);
    resolve(&mut document, 0.30);
    let after_transform = *document.inner().get_node(motion).unwrap().transform();
    let after_opacity = document
        .inner()
        .get_node(highlight)
        .unwrap()
        .primary_styles()
        .unwrap()
        .clone_opacity();
    assert_eq!(
        after_transform, before_transform,
        "the neutral computed transform itself did not return"
    );
    assert_eq!(
        after_opacity, before_opacity,
        "the neutral highlight opacity itself did not return"
    );
    assert_eq!(
        paint_order(&document),
        before_paint_order,
        "the neutral stacking-context paint order did not return"
    );
    let after = render(&mut document);

    let changed = before
        .chunks_exact(4)
        .zip(after.chunks_exact(4))
        .filter(|(left, right)| {
            left[..3]
                .iter()
                .zip(&right[..3])
                .any(|(a, b)| a.abs_diff(*b) > 8)
        })
        .count();
    assert_eq!(
        changed, 0,
        "{changed} visibly coloured pixel(s) changed after hover returned to neutral"
    );
}

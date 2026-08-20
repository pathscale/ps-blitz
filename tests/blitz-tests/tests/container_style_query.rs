//! `@container style(--token: value)` has to apply.
//!
//! A style container query is how a component library can make a material a
//! single global flip: components emit their default attribute, one custom
//! property on `:root` decides what that default means, and no call site is
//! touched. That only works if the engine implements the query. An engine that
//! ignores it silently keeps every component at its fallback, which looks
//! exactly like "the flip did nothing".
//!
//! Blitz has been wrongly suspected of dropping CSS features twice, so this
//! answers it by rendering rather than by reading the source.
//!
//!   cargo test --release -p blitz-tests --test container_style_query -- --nocapture

use anyrender::render_to_buffer;
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_dom::DocumentConfig;
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_paint::paint_scene;
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

const WIDTH: u32 = 80;
const HEIGHT: u32 = 40;

fn render(html: &str) -> Vec<u8> {
    let mut doc = HtmlDocument::from_html(
        html,
        DocumentConfig {
            viewport: Some(Viewport::new(WIDTH, HEIGHT, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| paint_scene(scene, &mut doc, 1.0, WIDTH, HEIGHT, 0, 0),
        WIDTH,
        HEIGHT,
    )
}

fn pixel(buffer: &[u8], x: u32, y: u32) -> [u8; 3] {
    let idx = ((y * WIDTH + x) * 4) as usize;
    [buffer[idx], buffer[idx + 1], buffer[idx + 2]]
}

/// Red is the ungated fill, green is what the style query admits.
#[test]
fn a_style_container_query_applies_when_the_token_matches() {
    let buffer = render(
        r#"<html><head><style>
             body { margin: 0; --flip: 1; }
             #box { width: 80px; height: 40px; background-color: rgb(255 0 0); }
             @container style(--flip: 1) {
               #box { background-color: rgb(0 255 0); }
             }
           </style></head><body><div id="box"></div></body></html>"#,
    );

    assert_eq!(
        pixel(&buffer, 40, 20),
        [0, 255, 0],
        "a style container query did not apply, so a token-driven material flip \
         silently does nothing"
    );
}

/// The other half: it must *not* apply when the token does not match, or the
/// flip is stuck on rather than merely broken.
#[test]
fn a_style_container_query_does_not_apply_when_the_token_differs() {
    let buffer = render(
        r#"<html><head><style>
             body { margin: 0; --flip: 0; }
             #box { width: 80px; height: 40px; background-color: rgb(255 0 0); }
             @container style(--flip: 1) {
               #box { background-color: rgb(0 255 0); }
             }
           </style></head><body><div id="box"></div></body></html>"#,
    );

    assert_eq!(
        pixel(&buffer, 40, 20),
        [255, 0, 0],
        "a style container query applied when its token did not match"
    );
}

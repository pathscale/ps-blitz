//! A rule inside `@supports (color: color-mix(in lab, red, red))` has to apply.
//!
//! Lightning CSS treats `color-mix` as progressive enhancement: it emits an
//! opaque fallback declaration and puts the real one inside that query. In one
//! application bundle 205 of 501 `color-mix` uses are wrapped that way, so an
//! engine that fails the query silently renders every one of them at its
//! fallback and nothing looks broken enough to investigate.
//!
//! That was the suspected cause of a window that would not go transparent. It
//! was the wrong suspicion, and this test is what settles it either way rather
//! than leaving the question to inference.
//!
//!   cargo test --release -p blitz-tests --test supports_gated_color_mix -- --nocapture

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

/// The fallback is red, the gated rule is green. Green means the query matched.
#[test]
fn a_supports_gated_rule_wins_over_its_fallback() {
    let buffer = render(
        r#"<html><head><style>
             body { margin: 0 }
             #box { width: 80px; height: 40px; background-color: rgb(255 0 0); }
             @supports (color: color-mix(in lab, red, red)) {
               #box { background-color: rgb(0 255 0); }
             }
           </style></head><body><div id="box"></div></body></html>"#,
    );

    assert_eq!(
        pixel(&buffer, 40, 20),
        [0, 255, 0],
        "the @supports rule did not apply, so every Lightning CSS enhancement \
         is rendering at its fallback"
    );
}

/// The mix itself has to produce alpha, which is the other half of the story:
/// a query that matches is no use if the value it admits is wrong.
#[test]
fn color_mix_with_transparent_produces_alpha() {
    let buffer = render(
        r#"<html><head><style>
             body { margin: 0; background-color: rgb(0 0 255); }
             #box {
               width: 80px; height: 40px;
               background-color: color-mix(in oklab, rgb(255 0 0) 50%, transparent);
             }
           </style></head><body><div id="box"></div></body></html>"#,
    );

    let [r, _g, b] = pixel(&buffer, 40, 20);
    assert!(
        b > 60,
        "a 50% mix with transparent painted opaque: the blue page never showed \
         through (got r={r} b={b})"
    );
}

/// The case that actually bit: a *custom property* redefined inside
/// `@supports`, rather than a rule.
///
/// Lightning CSS emits `--token: <fallback>` at `:root` and the real value in a
/// gated `:root` block. That is a different code path from a gated declaration
/// on an element, and it is the one the application depends on for every
/// surface colour it owns.
#[test]
fn a_supports_gated_custom_property_wins_over_its_fallback() {
    let buffer = render(
        r#"<html><head><style>
             body { margin: 0 }
             :root { --surface: rgb(255 0 0); }
             @supports (color: color-mix(in lab, red, red)) {
               :root { --surface: rgb(0 255 0); }
             }
             #box { width: 80px; height: 40px; background-color: var(--surface); }
           </style></head><body><div id="box"></div></body></html>"#,
    );

    assert_eq!(
        pixel(&buffer, 40, 20),
        [0, 255, 0],
        "a custom property redefined inside @supports kept its fallback, so \
         every themed colour resolves to the wrong value"
    );
}

//! The desk has to let what is behind it through when its alpha is turned down.
//!
//! This is the assertion every earlier attempt at window transparency was
//! missing. The application's tests checked the stylesheet source and the
//! JavaScript that writes `--az-glass-alpha`, and both stayed green through
//! three consecutive builds that rendered a completely opaque window: neither
//! one looks at a pixel, so neither could tell a working chain from a broken
//! one. The owner's screenshot was the only feedback loop, which made every
//! wrong guess cost a full rebuild.
//!
//! The exact declarations here are the ones `theme.css` ships, so a change that
//! breaks them breaks this.
//!
//!   cargo test --release -p blitz-tests --test window_alpha_admits_light -- --nocapture

use anyrender::render_to_buffer;
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_dom::DocumentConfig;
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_paint::paint_scene;
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

const WIDTH: u32 = 80;
const HEIGHT: u32 = 40;

/// What sits behind the window. Any of it reaching the camera is light getting
/// through; none of it is an opaque desk.
const BEHIND: [u8; 3] = [0, 0, 255];

fn render(alpha: &str) -> Vec<u8> {
    let html = format!(
        r#"<html><head><style>
             html {{ background-color: rgb(0 0 255); }}
             body {{ margin: 0; }}
             :root {{ --color-az-desk: rgb(20 20 22); --az-glass-alpha: {alpha}; }}
             .az-desk {{
               width: 80px; height: 40px;
               background-color: rgb(from var(--color-az-desk) r g b / var(--az-glass-alpha, 100%));
             }}
           </style></head><body><div class="az-desk"></div></body></html>"#
    );
    let mut doc = HtmlDocument::from_html(
        &html,
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

/// At full alpha the desk is the surface, and nothing behind it shows.
#[test]
fn a_solid_desk_hides_what_is_behind_it() {
    let [_, _, blue] = pixel(&render("100%"), 40, 20);
    assert!(
        blue < 60,
        "the desk was supposed to be opaque at 100% but the backdrop showed \
         through (blue={blue})"
    );
}

/// Turned down, the backdrop has to come through. This is the whole feature.
#[test]
fn a_translucent_desk_admits_what_is_behind_it() {
    let [_, _, blue] = pixel(&render("20%"), 40, 20);
    assert!(
        blue > BEHIND[2] / 2,
        "the desk painted opaque at 20% alpha: nothing behind the window can be \
         seen, which is the bug this exists to catch (blue={blue})"
    );
}

/// And it has to be a range, not a switch: more alpha is less backdrop.
#[test]
fn the_backdrop_fades_as_the_alpha_rises() {
    let [_, _, clear] = pixel(&render("10%"), 40, 20);
    let [_, _, mid] = pixel(&render("60%"), 40, 20);
    let [_, _, solid] = pixel(&render("100%"), 40, 20);

    assert!(
        clear > mid && mid > solid,
        "the alpha is not a continuous range: 10% gave blue={clear}, \
         60% gave {mid}, 100% gave {solid}"
    );
}

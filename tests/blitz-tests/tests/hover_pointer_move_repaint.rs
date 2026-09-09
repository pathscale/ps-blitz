//! Hover driven the way a host drives it: a `UiEvent::PointerMove`, then a
//! resolve, then a paint.
//!
//! `hover_media.rs` already proves `@media (hover: hover)` evaluates true and
//! that `set_hover_to` restyles. That is not the path a host takes. This is,
//! including the case a component library actually ships: the hover colour on a
//! CSS custom property, read by a rule on the element itself.

use anyrender::render_to_buffer;
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_dom::{Document, DocumentConfig};
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_paint::paint_scene;
use blitz_traits::events::{
    BlitzPointerEvent, BlitzPointerId, MouseEventButton, MouseEventButtons, Point, PointerCoords,
    PointerDetails, UiEvent,
};
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

fn pixel(doc: &mut HtmlDocument) -> [u8; 3] {
    let buffer = render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| paint_scene(scene, doc, 1.0, 200, 100, 0, 0),
        200,
        100,
    );
    let offset = 40 * 200 * 4 + 40 * 4;
    [buffer[offset], buffer[offset + 1], buffer[offset + 2]]
}

fn pointer_move(x: f32, y: f32) -> UiEvent {
    UiEvent::PointerMove(BlitzPointerEvent {
        id: BlitzPointerId::Mouse,
        is_primary: true,
        coords: PointerCoords {
            page_x: x,
            page_y: y,
            screen_x: x,
            screen_y: y,
            client_x: x,
            client_y: y,
        },
        button: MouseEventButton::Main,
        buttons: MouseEventButtons::empty(),
        mods: Default::default(),
        details: PointerDetails::default(),
        element: Point::default(),
        active_pointers: Default::default(),
    })
}

fn doc_from(html: &str) -> HtmlDocument {
    HtmlDocument::from_html(
        html,
        DocumentConfig {
            viewport: Some(Viewport::new(200, 100, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    )
}

#[test]
fn a_pointer_move_repaints_a_hover_rule() {
    let mut doc = doc_from(
        r#"<html><head><style>
          body { margin: 0; background: white }
          button { width: 80px; height: 80px; background: #0000ff }
          @media (hover: hover) { button:hover { background: #ff0000 } }
        </style></head><body><button><span>close</span></button></body></html>"#,
    );
    doc.resolve(0.0);
    assert_eq!(pixel(&mut doc), [0, 0, 255]);

    doc.handle_ui_event(pointer_move(40.0, 40.0));
    doc.resolve(0.0);
    assert_eq!(
        pixel(&mut doc),
        [255, 0, 0],
        "a pointer move must leave the hover rule in the next painted frame"
    );
}

#[test]
fn a_pointer_move_repaints_a_hover_rule_written_through_a_custom_property() {
    let mut doc = doc_from(
        r#"<html><head><style>
          body { margin: 0; background: white }
          :root { --button-bg: #0000ff; --button-bg-hover: #0000ff }
          @media (hover: hover) { :root { --button-bg-hover: #ff0000 } }
          button { width: 80px; height: 80px; background: var(--button-bg) }
          button:hover { background: var(--button-bg-hover) }
        </style></head><body><button><span>close</span></button></body></html>"#,
    );
    doc.resolve(0.0);
    assert_eq!(pixel(&mut doc), [0, 0, 255]);

    doc.handle_ui_event(pointer_move(40.0, 40.0));
    doc.resolve(0.0);
    assert_eq!(
        pixel(&mut doc),
        [255, 0, 0],
        "the hover colour arrives through a custom property gated on (hover: hover)"
    );
}

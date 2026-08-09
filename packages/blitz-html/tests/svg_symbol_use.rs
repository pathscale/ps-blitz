//! Same-document SVG fragment references used by icon sprites.
//!
//! Application bundles commonly keep reusable `<symbol>` elements in a hidden
//! sibling `<svg>` and instantiate them with `<use href="#...">`. Inline SVGs
//! are converted to replaced images by Blitz, so their serialized source must
//! include any referenced definitions from the surrounding document.

use anyrender::render_to_buffer;
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_dom::{DocumentConfig, LocalName, Namespace, QualName};
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_paint::paint_scene;
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

fn render(html: &str) -> Vec<u8> {
    render_at(html, 0, 0)
}

fn render_at(html: &str, x_offset: u32, y_offset: u32) -> Vec<u8> {
    let mut doc = HtmlDocument::from_html(
        html,
        DocumentConfig {
            viewport: Some(Viewport::new(40, 40, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| paint_scene(scene, &mut doc, 1.0, 40, 40, x_offset, y_offset),
        40,
        40,
    )
}

fn pixel(buffer: &[u8], x: usize, y: usize) -> [u8; 3] {
    let idx = (y * 40 + x) * 4;
    [buffer[idx], buffer[idx + 1], buffer[idx + 2]]
}

#[test]
fn use_resolves_symbol_from_sibling_sprite_and_inherits_current_color() {
    let buffer = render(
        r##"<html><body style="margin:0; background:white;">
            <svg width="0" height="0" style="position:absolute">
                <symbol id="i-square" viewBox="0 0 10 10">
                    <rect width="10" height="10" />
                </symbol>
            </svg>
            <svg width="20" height="20" viewBox="0 0 10 10"
                 style="color:rgb(220, 10, 20)" fill="currentColor">
                <use href="#i-square" />
            </svg>
        </body></html>"##,
    );

    assert_eq!(
        pixel(&buffer, 10, 10),
        [220, 10, 20],
        "a visible use must resolve its sibling symbol and inherit currentColor"
    );
}

#[test]
fn svg_paint_transform_includes_the_parent_view_translation() {
    let buffer = render_at(
        r##"<html><body style="margin:0; background:white;">
            <svg width="0" height="0" style="position:absolute">
                <symbol id="i-square" viewBox="0 0 10 10">
                    <rect width="10" height="10" />
                </symbol>
            </svg>
            <svg width="20" height="20" viewBox="0 0 10 10"
                 style="color:rgb(30, 90, 210)" fill="currentColor">
                <use href="#i-square" />
            </svg>
        </body></html>"##,
        10,
        8,
    );

    assert_eq!(pixel(&buffer, 20, 18), [30, 90, 210]);
}

#[test]
fn current_color_inside_imported_symbol_uses_visible_svg_color() {
    let buffer = render(
        r##"<html><body style="margin:0; color:rgb(20, 20, 20); background:white;">
            <svg width="0" height="0" style="position:absolute">
                <symbol id="i-current-color" viewBox="0 0 10 10">
                    <rect width="10" height="10" fill="currentColor" />
                </symbol>
            </svg>
            <svg width="20" height="20" viewBox="0 0 10 10"
                 style="color:rgb(20, 180, 40)">
                <use href="#i-current-color" />
            </svg>
        </body></html>"##,
    );

    assert_eq!(pixel(&buffer, 10, 10), [20, 180, 40]);
}

#[test]
fn imported_icon_inherits_visible_svg_stroke_attributes() {
    let buffer = render(
        r##"<html><body style="margin:0; background:white;">
            <svg width="0" height="0" style="position:absolute">
                <symbol id="i-line" viewBox="0 0 10 10">
                    <path d="M1 5h8" />
                </symbol>
            </svg>
            <svg width="20" height="20" viewBox="0 0 10 10"
                 style="color:rgb(190, 30, 80)" fill="none"
                 stroke="currentColor" stroke-width="2" stroke-linecap="round">
                <use href="#i-line" />
            </svg>
        </body></html>"##,
    );

    assert_eq!(pixel(&buffer, 10, 10), [190, 30, 80]);
}

#[test]
fn modern_css_current_color_is_converted_for_svg_painting() {
    let buffer = render(
        r##"<html><body style="margin:0; background:white;">
            <svg width="0" height="0" style="position:absolute">
                <symbol id="i-modern-color" viewBox="0 0 10 10">
                    <path d="M1 5h8" />
                </symbol>
            </svg>
            <svg width="20" height="20" viewBox="0 0 10 10"
                 style="color:oklab(0.66 -0.004 -0.009)" fill="none"
                 stroke="currentColor" stroke-width="2" stroke-linecap="round">
                <use href="#i-modern-color" />
            </svg>
        </body></html>"##,
    );

    assert_ne!(
        pixel(&buffer, 10, 10),
        [255, 255, 255],
        "modern computed colors must remain paintable after SVG preprocessing"
    );
}

#[test]
fn missing_local_reference_is_non_fatal() {
    let buffer = render(
        r##"<html><body style="margin:0; background:white;">
            <svg width="20" height="20" viewBox="0 0 10 10">
                <use href="#missing" />
            </svg>
        </body></html>"##,
    );

    assert_eq!(pixel(&buffer, 10, 10), [255, 255, 255]);
}

#[test]
fn referenced_symbol_can_use_another_document_local_symbol() {
    let buffer = render(
        r##"<html><body style="margin:0; background:white;">
            <svg width="0" height="0" style="position:absolute">
                <symbol id="i-base" viewBox="0 0 10 10">
                    <rect width="10" height="10" fill="rgb(10, 30, 210)" />
                </symbol>
                <symbol id="i-nested" viewBox="0 0 10 10">
                    <use href="#i-base" />
                </symbol>
            </svg>
            <svg width="20" height="20" viewBox="0 0 10 10">
                <use href="#i-nested" />
            </svg>
        </body></html>"##,
    );

    assert_eq!(pixel(&buffer, 10, 10), [10, 30, 210]);
}

#[test]
fn cyclic_symbol_references_do_not_loop_or_paint() {
    let buffer = render(
        r##"<html><body style="margin:0; background:white;">
            <svg width="0" height="0" style="position:absolute">
                <symbol id="i-a" viewBox="0 0 10 10"><use href="#i-b" /></symbol>
                <symbol id="i-b" viewBox="0 0 10 10"><use href="#i-a" /></symbol>
            </svg>
            <svg width="20" height="20" viewBox="0 0 10 10">
                <use href="#i-a" />
            </svg>
        </body></html>"##,
    );

    assert_eq!(pixel(&buffer, 10, 10), [255, 255, 255]);
}

#[test]
fn href_set_after_use_is_mounted_reconstructs_the_svg_image() {
    let mut doc = HtmlDocument::from_html(
        r##"<html><body style="margin:0; background:white;">
            <svg width="0" height="0" style="position:absolute">
                <symbol id="i-late" viewBox="0 0 10 10">
                    <rect width="10" height="10" />
                </symbol>
            </svg>
            <svg width="20" height="20" viewBox="0 0 10 10"
                 style="color:rgb(30, 80, 210)" fill="currentColor">
                <use id="late-use" />
            </svg>
        </body></html>"##,
        DocumentConfig {
            viewport: Some(Viewport::new(40, 40, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);

    let use_id = doc.query_selector("#late-use").unwrap().unwrap();
    doc.mutate().set_attribute(
        use_id,
        QualName {
            prefix: None,
            ns: Namespace::from(""),
            local: LocalName::from("href"),
        },
        "#i-late",
    );
    doc.resolve(0.0);

    let buffer = render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| paint_scene(scene, &mut doc, 1.0, 40, 40, 0, 0),
        40,
        40,
    );
    assert_eq!(pixel(&buffer, 10, 10), [30, 80, 210]);
}

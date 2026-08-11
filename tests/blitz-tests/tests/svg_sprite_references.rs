//! An inline SVG must carry the symbols it references with it.
//!
//! Blitz paints inline SVG as a replaced image: the element's markup is
//! serialised and handed to usvg, which sees that string and nothing else. An
//! icon sprite is the common case where that is not enough — the visible
//! `<svg><use href="#icon"></svg>` is a few bytes, and the geometry lives in a
//! separate `<svg>` elsewhere in the document. Serialise only the subtree and
//! usvg has no `#icon` to resolve, so the icon is silently not drawn.

use blitz_dom::DocumentConfig;
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

fn document(html: &str) -> HtmlDocument {
    let mut doc = HtmlDocument::from_html(
        html,
        DocumentConfig {
            viewport: Some(Viewport::new(400, 300, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    doc
}

#[test]
fn a_referenced_sprite_symbol_travels_with_the_svg() {
    let doc = document(
        r##"<html><body>
            <svg id="sprite" style="display:none">
              <symbol id="icon-star" viewBox="0 0 10 10">
                <path d="M0 0 L10 10"/>
              </symbol>
            </svg>
            <svg id="use-site" width="20" height="20">
              <use href="#icon-star"/>
            </svg>
        </body></html>"##,
    );

    let id = doc.query_selector("#use-site").unwrap().unwrap();
    let source = doc
        .debug_inline_svg_source(id)
        .expect("an svg element must serialise");

    assert!(
        source.contains("icon-star") && source.contains("M0 0 L10 10"),
        "the referenced symbol's geometry must be imported: {source}"
    );
    assert!(
        source.contains("<defs>"),
        "imported definitions belong in a generated <defs>: {source}"
    );
    // usvg refuses to parse without the namespace on the root.
    assert!(
        source.starts_with("<svg xmlns=\"http://www.w3.org/2000/svg\""),
        "the root must carry the SVG namespace: {source}"
    );
}

#[test]
fn a_case_sensitive_attribute_survives_serialisation() {
    // `outer_html` lowercases attribute names, which is right for HTML and
    // wrong here: SVG is case sensitive, so a `viewBox` serialised as `viewbox`
    // is ignored, usvg falls back to the bounding box of the path geometry, and
    // the intrinsic aspect ratio comes out wrong.
    let doc = document(
        r##"<html><body>
            <svg id="chart" viewBox="0 0 100 50"><path d="M0 0 L100 50"/></svg>
        </body></html>"##,
    );

    let id = doc.query_selector("#chart").unwrap().unwrap();
    let source = doc.debug_inline_svg_source(id).unwrap();

    assert!(
        source.contains("viewBox=\"0 0 100 50\""),
        "viewBox must keep its casing: {source}"
    );
}

#[test]
fn a_reference_already_inside_the_svg_is_not_imported_twice() {
    // Importing it again would duplicate the id, and usvg resolves whichever it
    // sees first.
    let doc = document(
        r##"<html><body>
            <svg id="self-contained" width="20" height="20">
              <defs><path id="local-path" d="M1 1 L9 9"/></defs>
              <use href="#local-path"/>
            </svg>
        </body></html>"##,
    );

    let id = doc.query_selector("#self-contained").unwrap().unwrap();
    let source = doc.debug_inline_svg_source(id).unwrap();

    assert_eq!(
        source.matches("local-path").count(),
        2,
        "the id should appear once on the definition and once on the use, not \
         a third time from an import: {source}"
    );
}

/// How many nodes the *cached* usvg tree holds.
///
/// The cache is what actually gets painted, and it is what the reconstruction
/// hook exists to invalidate. `debug_inline_svg_source` serialises live from
/// the DOM and would report a change whether or not the cache was rebuilt, so
/// it cannot see this bug.
fn cached_svg_node_count(doc: &HtmlDocument, node_id: blitz_dom::NodeId) -> usize {
    doc.get_node(node_id)
        .unwrap()
        .element_data()
        .unwrap()
        .svg_data()
        .map(|tree| tree.root().children().len())
        .unwrap_or(0)
}

#[test]
fn setting_a_use_href_later_rebuilds_the_cached_svg() {
    // The reconstruction hook this guards had lost both its call sites.
    //
    // Inline SVG is cached as a replaced image. Frameworks commonly append
    // `<use>` first and set its `href` in a later DOM operation, so rebuilding
    // only the changed child leaves the root image cached from the empty,
    // pre-attribute source — an icon that never appears no matter what the
    // markup ends up saying.
    let mut doc = document(
        r##"<html><body>
            <svg id="sprite" style="display:none">
              <symbol id="icon-bolt" viewBox="0 0 10 10"><path d="M2 2 L8 8"/></symbol>
            </svg>
            <svg id="use-site" width="20" height="20"><use/></svg>
        </body></html>"##,
    );

    let use_site = doc.query_selector("#use-site").unwrap().unwrap();
    let use_el = doc.query_selector("#use-site use").unwrap().unwrap();

    assert_eq!(
        cached_svg_node_count(&doc, use_site),
        0,
        "nothing is referenced yet, so the cached tree should be empty"
    );

    let mut mutator = doc.mutate();
    mutator.set_attribute(use_el, blitz_dom::qual_name!("href", html), "#icon-bolt");
    drop(mutator);
    doc.resolve(0.0);

    assert!(
        cached_svg_node_count(&doc, use_site) > 0,
        "setting href later must rebuild the cached image, not leave it at the \
         pre-attribute source"
    );
}

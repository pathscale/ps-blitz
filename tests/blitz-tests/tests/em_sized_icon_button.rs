//! An `em`-sized SVG icon inside a fixed-size button must not abort layout.
//!
//! This is the markup AgencyZero 0.6.1 died on, printed by
//! `BLITZ_TRACE_LAYOUT_PANIC=1` out of the running app: the copy button beside
//! the session id in the project header.
//!
//! ```text
//! stylo-0.20.0/values/computed/length_percentage.rs:654:14:
//! called `Result::unwrap()` on an `Err` value: ()
//! ```
//!
//! It killed the process a couple of seconds after `boot: ready`, and not
//! always the same way: a Rust panic, a `SIGSEGV`, and a silent `SIGABRT` all
//! came out of the same markup on different launches, which is why the stylo
//! line number was a poor guide on its own.

use blitz_dom::DocumentConfig;
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

const CSS: &str = include_str!("../fixtures/app.css");

fn resolve(body: &str) -> HtmlDocument {
    let html = format!(
        r##"<html><head><style>{CSS}</style></head><body class="bg-base-100" style="margin:0">{body}</body></html>"##
    );
    let mut doc = HtmlDocument::from_html(
        &html,
        DocumentConfig {
            viewport: Some(Viewport::new(1344, 900, 1.0, ColorScheme::Dark)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.set_incremental_layout(true);
    doc.resolve(0.0);
    doc
}

/// Verbatim, sprite reference included.
#[test]
fn the_session_copy_button_lays_out() {
    let doc = resolve(
        r##"<svg style="display:none"><symbol id="i-copy" viewBox="0 0 24 24"><rect x="9" y="9" width="13" height="13" rx="2"/></symbol></svg>
           <button id="b" type="button" style="display:flex; width:18px; height:18px; align-items:center; justify-content:center" aria-label="Copy session id">
             <svg viewBox="0 0 24 24" width="1em" height="1em" fill="none" stroke="rgba(120, 124, 125, 1)" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" style="font-size:10px" role="presentation" aria-hidden="true">
               <use href="#i-copy" />
             </svg>
           </button>"##,
    );
    let button = doc.query_selector("#b").unwrap().expect("no button");
    let layout = doc.get_node(button).unwrap().final_layout();
    assert!(
        layout.size.width > 0.0 && layout.size.height > 0.0,
        "button laid out as {}x{}",
        layout.size.width,
        layout.size.height
    );
}

/// The `em` sizing alone, with no sprite reference, so a failure above can be
/// attributed to one or the other.
#[test]
fn an_em_sized_svg_lays_out_without_a_sprite() {
    let doc = resolve(
        r##"<button id="b" type="button" style="display:flex; width:18px; height:18px">
             <svg id="s" viewBox="0 0 24 24" width="1em" height="1em" style="font-size:10px"><rect width="10" height="10"/></svg>
           </button>"##,
    );
    let svg = doc.query_selector("#s").unwrap().expect("no svg");
    assert!(doc.get_node(svg).unwrap().final_layout().size.width > 0.0);
}

/// A `use` reference with no `em` sizing, for the same reason.
#[test]
fn a_sprite_reference_lays_out_without_em_sizing() {
    let doc = resolve(
        r##"<svg style="display:none"><symbol id="i-copy" viewBox="0 0 24 24"><rect x="9" y="9" width="13" height="13" rx="2"/></symbol></svg>
           <button id="b" type="button" style="display:flex; width:18px; height:18px">
             <svg id="s" viewBox="0 0 24 24" width="10px" height="10px"><use href="#i-copy" /></svg>
           </button>"##,
    );
    let svg = doc.query_selector("#s").unwrap().expect("no svg");
    assert!(doc.get_node(svg).unwrap().final_layout().size.width > 0.0);
}

/// The class lists as the application writes them, against the stylesheet it
/// embeds: `size-[18px]` and `text-[10px]` are what set the button's box and
/// the icon's `em` basis, and `transition-colors` is what makes the button an
/// animating element while it is doing so.
#[test]
fn the_button_lays_out_with_the_applications_own_classes() {
    let doc = resolve(
        r##"<svg style="display:none"><symbol id="i-copy" viewBox="0 0 24 24"><rect x="9" y="9" width="13" height="13" rx="2"/></symbol></svg>
           <span class="inline-flex items-center gap-1.5 font-mono text-[11px] text-az-muted">
             <span>session · e4477b88</span>
             <button id="b" type="button" class="flex size-[18px] items-center justify-center rounded transition-colors hover:bg-white/8 hover:text-az-body" aria-label="Copy session id">
               <svg viewBox="0 0 24 24" width="1em" height="1em" fill="none" stroke="rgba(120, 124, 125, 1)" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" class="text-[10px]" role="presentation" aria-hidden="true">
                 <use href="#i-copy" />
               </svg>
             </button>
           </span>"##,
    );
    let button = doc.query_selector("#b").unwrap().expect("no button");
    let layout = doc.get_node(button).unwrap().final_layout();
    assert!(
        layout.size.width > 0.0 && layout.size.height > 0.0,
        "button laid out as {}x{}",
        layout.size.width,
        layout.size.height
    );
}

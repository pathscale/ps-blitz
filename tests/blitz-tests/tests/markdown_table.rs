//! A markdown table in a message must not take the process down.
//!
//! `MessageBody` renders a `| a | b |` block as a real `<table class="w-full
//! border-collapse">` inside an `overflow-x-auto` wrapper, so an agent that
//! answers with a table puts `display: table` and `width: 100%` into the
//! transcript. 0.6.1 aborted a couple of seconds after `boot: ready`, and a
//! sweep of the shipped stylesheet found `table-row` and `table-column` were
//! the only two classes in it that abort layout on their own.

use blitz_dom::DocumentConfig;
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

fn resolve(html: &str) -> HtmlDocument {
    let mut doc = HtmlDocument::from_html(
        html,
        DocumentConfig {
            viewport: Some(Viewport::new(1344, 900, 1.0, ColorScheme::Dark)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    doc
}

/// Exactly what `TableBlock` emits, structure and widths included.
#[test]
fn a_message_table_lays_out() {
    let doc = resolve(
        r#"<html><body style="margin:0">
             <div id="wrap" style="overflow-x:auto; border-radius:8px; border:1px solid #333">
               <table id="t" style="width:100%; border-collapse:collapse; font-size:12px">
                 <thead><tr>
                   <th style="border-bottom:1px solid #333; padding:6px 12px">build</th>
                   <th style="border-bottom:1px solid #333; padding:6px 12px">CPU time</th>
                 </tr></thead>
                 <tbody>
                   <tr><td style="padding:6px 12px">blitz-runtime</td><td style="padding:6px 12px">0.13s</td></tr>
                   <tr><td style="padding:6px 12px">blitz-inspector</td><td style="padding:6px 12px">0.09s</td></tr>
                 </tbody>
               </table>
             </div>
           </body></html>"#,
    );
    let table = doc.query_selector("#t").unwrap().expect("no table");
    let layout = doc.get_node(table).unwrap().final_layout();
    assert!(
        layout.size.width > 0.0 && layout.size.height > 0.0,
        "table laid out as {}x{}",
        layout.size.width,
        layout.size.height
    );
}

/// The width alone, with no table around it, so a failure above can be
/// attributed to `display: table` rather than to the percentage.
#[test]
fn a_percentage_width_block_lays_out() {
    let doc = resolve(
        r#"<html><body style="margin:0"><div id="t" style="width:100%">x</div></body></html>"#,
    );
    let t = doc.query_selector("#t").unwrap().expect("no node");
    assert!(doc.get_node(t).unwrap().final_layout().size.width > 0.0);
}

/// `display: table-row` and `display: table-column` on their own, which is what
/// the stylesheet sweep flagged.
#[test]
fn the_table_display_keywords_lay_out() {
    for display in ["table", "table-row", "table-column", "table-cell"] {
        resolve(&format!(
            r#"<html><body style="margin:0"><div style="display:{display}"><span>text</span></div></body></html>"#
        ));
    }
}

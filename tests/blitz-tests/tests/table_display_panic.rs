use blitz_dom::DocumentConfig;
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

const CSS: &str = include_str!("../fixtures/app.css");

fn go(class: &str) {
    let html = format!(
        r#"<html><head><style>{CSS}</style></head>
           <body class="bg-base-100" style="margin:0">
             <div class="{class}"><span class="{class}">text</span></div>
           </body></html>"#
    );
    let mut doc = HtmlDocument::from_html(
        &html,
        DocumentConfig {
            viewport: Some(Viewport::new(1344, 900, 1.0, ColorScheme::Dark)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
}

#[test]
fn table_row() { go("table-row"); }

#[test]
fn table_column() { go("table-column"); }

/// Without the stylesheet, so the answer is `display` and not some other
/// declaration the class happens to carry.
#[test]
fn bare_display_table_row() {
    let html = r#"<html><body style="margin:0"><div style="display:table-row"><span>text</span></div></body></html>"#;
    let mut doc = HtmlDocument::from_html(
        html,
        DocumentConfig {
            viewport: Some(Viewport::new(1344, 900, 1.0, ColorScheme::Dark)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
}

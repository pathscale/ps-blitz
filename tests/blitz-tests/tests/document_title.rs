use blitz_dom::DocumentConfig;
use blitz_html::HtmlDocument;

#[test]
fn svg_title_does_not_replace_the_html_document_title() {
    let document = HtmlDocument::from_html(
        r#"
        <!doctype html>
        <html>
          <head><title>AgencyZero</title></head>
          <body>
            <svg aria-hidden="true">
              <title>Icon sprite</title>
              <symbol id="settings"><path d="M0 0h1v1z" /></symbol>
            </svg>
          </body>
        </html>
        "#,
        DocumentConfig::default(),
    );

    let title = document
        .find_title_node()
        .expect("HTML document title")
        .text_content();
    assert_eq!(title, "AgencyZero");
}

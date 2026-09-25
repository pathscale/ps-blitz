//! `clientWidth` / `clientHeight` are the padding box, not the border box.
//!
//! They returned the border box, so a bordered element reported its borders
//! as usable space, and layout code that sizes children to `clientWidth`
//! overflowed by exactly the border.

use blitz_dom::Document as _;
use blitz_script::ScriptDocument;

#[test]
fn client_size_excludes_the_border() {
    let mut doc = ScriptDocument::from_html(
        r#"<html><head><style>
             #box { width: 200px; height: 50px; padding: 10px 12px; border: 3px solid black; }
           </style></head>
           <body style="margin:0"><div id="box"></div></body></html>"#,
        blitz_dom::DocumentConfig::default(),
    );
    doc.execute_scripts();
    doc.inner_mut().resolve(0.0);

    let size = doc
        .eval_json(
            "(() => { const b = document.getElementById('box');
                      return [b.clientWidth, b.clientHeight, b.offsetWidth, b.offsetHeight]; })()",
        )
        .expect("box metrics should evaluate");

    // Content 200x50 plus padding 24x20 is the padding box; the 3px border on
    // each side is only in the offset (border-box) size.
    assert_eq!(size, serde_json::json!([224, 70, 230, 76]));
}

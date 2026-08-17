//! `offsetWidth` / `offsetHeight` / `offsetTop` / `offsetLeft`.
//!
//! These were absent, so reading one produced `undefined` rather than a
//! number, and `undefined` in arithmetic is `NaN` rather than an error. Every
//! slider in the app went through this line of its library:
//!
//!   thumbRef.offsetWidth / 2 + Math.max(0, (trackRef.offsetHeight - thumbRef.offsetHeight) / 2)
//!
//! which is `NaN`, so the usable track length was `NaN`, the fraction along it
//! was `NaN`, and the value snapped from it was `NaN`. A slider whose value is
//! `NaN` renders with the thumb at a `calc()` that resolves to nothing and
//! ignores every drag: not visibly broken, just inert.

use blitz_dom::{Document, DocumentConfig};
use blitz_script::ScriptDocument;
use blitz_traits::shell::{ColorScheme, Viewport};

fn make_doc(html: &str) -> ScriptDocument {
    let mut doc = ScriptDocument::from_html(
        html,
        DocumentConfig {
            viewport: Some(Viewport::new(400, 300, 1.0, ColorScheme::Light)),
            ..Default::default()
        },
    );
    doc.execute_scripts();
    doc.inner_mut().resolve(0.0);
    doc
}

fn text_of_selector(doc: &ScriptDocument, selector: &str) -> String {
    let inner = doc.inner();
    let node_id = inner
        .query_selector(selector)
        .unwrap()
        .unwrap_or_else(|| panic!("no node matching {selector}"));
    inner.get_node(node_id).unwrap().text_content()
}

/// The border box, so padding and border count and margin does not.
#[test]
fn offset_size_is_the_border_box() {
    let doc = make_doc(
        r#"
        <html><head><style>
          body { margin: 0; }
          #box {
            width: 100px; height: 40px;
            padding: 5px; border: 2px solid black; margin: 9px;
            box-sizing: content-box;
          }
        </style></head>
        <body>
          <div id="box"></div>
          <div id="out"></div>
          <script>
            const box = document.getElementById("box");
            document.getElementById("out").textContent =
              `${box.offsetWidth}x${box.offsetHeight}`;
          </script>
        </body></html>
        "#,
    );

    // 100 + 2*5 padding + 2*2 border = 114, and 40 + 10 + 4 = 54.
    assert_eq!(text_of_selector(&doc, "#out"), "114x54");
}

/// The position within the offset parent, which is what a knob or a popover
/// places itself against.
#[test]
fn offset_position_is_relative_to_the_offset_parent() {
    let doc = make_doc(
        r#"
        <html><head><style>
          body { margin: 0; }
          #parent { position: relative; left: 30px; top: 40px; width: 200px; height: 100px; }
          #child { position: absolute; left: 12px; top: 7px; width: 20px; height: 20px; }
        </style></head>
        <body>
          <div id="parent"><div id="child"></div></div>
          <div id="out"></div>
          <script>
            const child = document.getElementById("child");
            document.getElementById("out").textContent =
              `${child.offsetLeft},${child.offsetTop}`;
          </script>
        </body></html>
        "#,
    );

    assert_eq!(text_of_selector(&doc, "#out"), "12,7");
}

/// The whole point: the library slider's inset calculation has to produce a
/// number. This is that expression, verbatim.
#[test]
fn the_slider_inset_expression_is_a_number() {
    let doc = make_doc(
        r#"
        <html><head><style>
          body { margin: 0; }
          #track { width: 200px; height: 20px; }
          #thumb { width: 16px; height: 16px; }
        </style></head>
        <body>
          <div id="track"><div id="thumb"></div></div>
          <div id="out"></div>
          <script>
            const track = document.getElementById("track");
            const thumb = document.getElementById("thumb");
            const pad = Math.max(0, (track.offsetHeight - thumb.offsetHeight) / 2);
            const inset = thumb.offsetWidth / 2 + pad;
            const usable = track.getBoundingClientRect().width - 2 * inset;
            document.getElementById("out").textContent =
              `${Number.isNaN(inset) ? "NaN" : inset}|${Number.isNaN(usable) ? "NaN" : usable}`;
          </script>
        </body></html>
        "#,
    );

    // inset = 16/2 + (20-16)/2 = 10, usable = 200 - 20 = 180.
    assert_eq!(text_of_selector(&doc, "#out"), "10|180");
}

/// A display:none element measures zero, rather than throwing or reporting the
/// size it would have had.
#[test]
fn a_hidden_element_measures_zero() {
    let doc = make_doc(
        r#"
        <html><head><style>
          #gone { display: none; width: 100px; height: 40px; }
        </style></head>
        <body>
          <div id="gone"></div>
          <div id="out"></div>
          <script>
            const gone = document.getElementById("gone");
            document.getElementById("out").textContent =
              `${gone.offsetWidth}x${gone.offsetHeight}`;
          </script>
        </body></html>
        "#,
    );

    assert_eq!(text_of_selector(&doc, "#out"), "0x0");
}

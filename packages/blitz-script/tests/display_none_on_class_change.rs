//! Setting `display: none` from script must take the box away.
//!
//! The application keeps its tabs mounted and swaps a class: the active one
//! gets `flex`, the others get `hidden`, which is `display: none`. On a live
//! instance both the project transcript and the settings page were laid out in
//! the same band at once, one painted over the other, and the panel read as
//! blank.

use blitz_dom::Document as _;
use blitz_script::ScriptDocument;

fn area(doc: &ScriptDocument, selector: &str) -> f32 {
    let doc = doc.inner();
    let Ok(Some(id)) = doc.query_selector(selector) else {
        return -1.0;
    };
    let layout = doc.get_node(id).unwrap().final_layout();
    layout.size.width * layout.size.height
}

#[test]
fn hiding_a_sibling_by_class_takes_its_box_away() {
    let mut doc = ScriptDocument::from_html(
        r#"<html><head><style>.shown{display:flex;flex:1} .hidden{display:none}</style></head>
           <body style="margin:0">
             <div style="display:flex; width:800px; height:600px;">
               <div id="a" class="shown">A</div>
               <div id="b" class="hidden">B</div>
             </div>
           </body></html>"#,
        Default::default(),
    );
    doc.poll(None);
    doc.inner_mut().resolve(0.0);
    assert!(area(&doc, "#a") > 0.0, "the shown tab never laid out");
    assert_eq!(area(&doc, "#b"), 0.0, "the hidden tab started with a box");

    // Swap them, the way switching tabs does.
    doc.eval(
        "document.getElementById('a').className = 'hidden';\
         document.getElementById('b').className = 'shown';",
    );
    doc.poll(None);
    doc.inner_mut().resolve(0.0);

    assert!(
        area(&doc, "#b") > 0.0,
        "the newly shown tab did not lay out"
    );
    assert_eq!(
        area(&doc, "#a"),
        0.0,
        "the newly hidden tab kept its box, so both tabs occupy the panel"
    );
}

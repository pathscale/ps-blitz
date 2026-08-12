//! A node removed from the document must stop occupying space.
//!
//! `removeChild` detaches rather than drops, so JS wrappers stay valid. The
//! question this asks is whether the detached node also leaves *layout*.
//!
//! It did not. The application's boot splash is a full-window element that
//! Solid removes once the workspace is ready. On a live instance the log showed
//! exactly one "splash mounted" and no "splash unmounted", and the node was
//! still laid out at 1318x880 over every panel for the rest of the session,
//! painting its own background into whichever one it landed in. That is the
//! blank page, and it also kept the document animating, because the splash
//! carries an infinite CSS animation.

use blitz_dom::Document as _;
use blitz_script::ScriptDocument;

fn document(script: &str) -> ScriptDocument {
    let html = format!(
        r#"<html><body style="margin:0">
             <div id="host" style="width:800px; height:600px;">
               <div id="splash" style="width:100%; height:100%;">Loading…</div>
             </div>
             <script>{script}</script>
           </body></html>"#
    );
    let mut doc = ScriptDocument::from_html(&html, Default::default());
    doc.poll(None);
    doc.inner_mut().resolve(0.0);
    doc
}

/// The area of a node held by id.
///
/// By id, not by selector. Looking it up again after removal returns `None`,
/// and "not found" then reads as "has no box", which is exactly the answer this
/// test exists to distrust: the node can be gone from the document and still be
/// laid out, which is the bug.
fn area_of(doc: &ScriptDocument, id: blitz_dom::NodeId) -> f32 {
    let doc = doc.inner();
    let Some(node) = doc.get_node(id) else {
        return 0.0;
    };
    let layout = node.final_layout();
    layout.size.width * layout.size.height
}

#[test]
fn a_removed_child_stops_occupying_space() {
    let mut doc = document("");
    let splash = doc
        .inner()
        .query_selector("#splash")
        .unwrap()
        .expect("no splash");
    assert!(area_of(&doc, splash) > 0.0, "the fixture never laid it out");

    doc.eval(
        "var host = document.getElementById('host');\
         host.removeChild(document.getElementById('splash'));",
    );
    doc.poll(None);
    doc.inner_mut().resolve(0.0);

    assert_eq!(
        area_of(&doc, splash),
        0.0,
        "the removed splash still has a layout box"
    );
}

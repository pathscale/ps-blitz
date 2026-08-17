//! A captured pointer drag survives leaving the element, through the real
//! event path.
//!
//! This is the shape every slider, knob and splitter uses: `pointerdown` calls
//! `setPointerCapture`, and from then on `pointermove` must reach the
//! capturing element wherever the pointer actually is, including outside its
//! box and outside the window.
//!
//! The existing capture test dispatches `DomEvent`s straight at a node, which
//! assumes the answer: it hands the move to the element under test rather than
//! asking what the engine would have targeted. These drive `handle_ui_event`
//! with coordinates instead, so hit testing happens for real.

use blitz_dom::{Document, DocumentConfig};
use blitz_script::ScriptDocument;
use blitz_traits::{
    events::{
        BlitzPointerEvent, BlitzPointerId, MouseEventButton, MouseEventButtons, Point,
        PointerCoords, PointerDetails, UiEvent,
    },
    shell::{ColorScheme, Viewport},
};

/// A track with a thumb, wired the way a library slider wires one: capture on
/// down, report every move, release on up.
const SLIDER: &str = r#"
<html><head><style>
  body { margin: 0; }
  #track { position: absolute; left: 0; top: 0; width: 200px; height: 20px; }
  #away { position: absolute; left: 0; top: 100px; width: 200px; height: 50px; }
</style></head>
<body>
  <div id="track"></div>
  <div id="away"></div>
  <div id="out"></div>
  <script>
    const track = document.getElementById("track");
    const out = document.getElementById("out");
    let moves = 0;
    track.addEventListener("pointerdown", (event) => {
      track.setPointerCapture(event.pointerId);
      out.textContent = "down";
    });
    track.addEventListener("pointermove", (event) => {
      moves += 1;
      out.textContent = `moves:${moves}:${event.clientX}`;
    });
    track.addEventListener("pointerup", (event) => {
      track.releasePointerCapture(event.pointerId);
      out.textContent += "|up";
    });
  </script>
</body></html>
"#;

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

fn pointer_event(x: f32, y: f32, buttons: MouseEventButtons) -> BlitzPointerEvent {
    BlitzPointerEvent {
        id: BlitzPointerId::Mouse,
        is_primary: true,
        coords: PointerCoords {
            page_x: x,
            page_y: y,
            screen_x: x,
            screen_y: y,
            client_x: x,
            client_y: y,
        },
        button: MouseEventButton::Main,
        buttons,
        mods: Default::default(),
        details: PointerDetails::default(),
        element: Point::default(),
        active_pointers: Default::default(),
    }
}

fn down(doc: &mut ScriptDocument, x: f32, y: f32) {
    let held = MouseEventButtons::from(MouseEventButton::Main);
    doc.handle_ui_event(UiEvent::PointerDown(pointer_event(x, y, held)));
}

fn move_to(doc: &mut ScriptDocument, x: f32, y: f32) {
    let held = MouseEventButtons::from(MouseEventButton::Main);
    doc.handle_ui_event(UiEvent::PointerMove(pointer_event(x, y, held)));
}

fn up(doc: &mut ScriptDocument, x: f32, y: f32) {
    doc.handle_ui_event(UiEvent::PointerUp(pointer_event(
        x,
        y,
        MouseEventButtons::None,
    )));
}

/// Dragging along the track reports every move. This is the case that works
/// even without capture, and it is here to prove the fixture is wired.
#[test]
fn a_drag_inside_the_element_reports_moves() {
    let mut doc = make_doc(SLIDER);
    down(&mut doc, 10.0, 10.0);
    move_to(&mut doc, 60.0, 10.0);
    assert_eq!(text_of_selector(&doc, "#out"), "moves:1:60");
}

/// The real case: the pointer leaves the track vertically, as it does whenever
/// someone drags a slider with any imprecision at all. Capture means the track
/// keeps receiving the moves.
#[test]
fn a_captured_drag_keeps_reporting_after_leaving_the_element() {
    let mut doc = make_doc(SLIDER);
    down(&mut doc, 10.0, 10.0);
    // Down over the track, then out of it: over a different element, and then
    // over no element at all.
    move_to(&mut doc, 60.0, 120.0);
    move_to(&mut doc, 120.0, 260.0);

    assert_eq!(
        text_of_selector(&doc, "#out"),
        "moves:2:120",
        "a captured pointer must keep delivering moves to the capturing element"
    );
}

/// Releasing capture stops the retargeting, so the element goes quiet.
#[test]
fn releasing_capture_stops_the_drag() {
    let mut doc = make_doc(SLIDER);
    down(&mut doc, 10.0, 10.0);
    move_to(&mut doc, 60.0, 120.0);
    up(&mut doc, 60.0, 120.0);
    move_to(&mut doc, 90.0, 120.0);

    assert_eq!(
        text_of_selector(&doc, "#out"),
        "moves:1:60|up",
        "after the release the track should hear nothing more"
    );
}

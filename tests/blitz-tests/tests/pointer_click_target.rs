use std::sync::{Arc, Mutex};

use blitz_dom::{Document, DocumentConfig, EventDriver, EventHandler, NodeId};
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_traits::{
    events::{
        BlitzPointerEvent, BlitzPointerId, DomEvent, DomEventData, EventState, MouseEventButton,
        MouseEventButtons, Point, PointerCoords, PointerDetails, UiEvent,
    },
    shell::{ColorScheme, Viewport},
};

#[derive(Clone, Default)]
struct ClickRecorder(Arc<Mutex<Vec<NodeId>>>);

impl EventHandler for ClickRecorder {
    fn handle_event(
        &mut self,
        _chain: &[NodeId],
        event: &mut DomEvent,
        _doc: &mut dyn Document,
        _event_state: &mut EventState,
    ) {
        if matches!(event.data, DomEventData::Click(_)) {
            self.0.lock().unwrap().push(event.target);
        }
    }
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

fn make_doc() -> HtmlDocument {
    let mut doc = HtmlDocument::from_html(
        r#"<!doctype html><html><body style="margin:0">
            <button id="warning-close" style="display:block;width:200px;height:40px">Close warning</button>
            <button id="pr-close" style="display:block;width:200px;height:40px">Close PR</button>
        </body></html>"#,
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
fn releasing_over_a_newly_exposed_control_does_not_click_it() {
    let mut doc = make_doc();
    let warning = doc.query_selector("#warning-close").unwrap().unwrap();
    let pr = doc.query_selector("#pr-close").unwrap().unwrap();
    let recorder = ClickRecorder::default();

    EventDriver::new(&mut doc, recorder.clone()).handle_ui_event(UiEvent::PointerDown(
        pointer_event(20.0, 20.0, MouseEventButtons::from(MouseEventButton::Main)),
    ));
    assert_eq!(doc.get_mousedown_node_id(), Some(warning));

    // The pressed warning disappears and layout moves the unrelated PR close
    // button under the unchanged pointer before the physical release arrives.
    doc.mutate().remove_and_drop_node(warning);
    doc.resolve(0.0);
    assert_eq!(doc.hit(20.0, 20.0).unwrap().node_id, pr);

    EventDriver::new(&mut doc, recorder.clone()).handle_ui_event(UiEvent::PointerUp(
        pointer_event(20.0, 20.0, MouseEventButtons::None),
    ));

    assert!(
        recorder.0.lock().unwrap().is_empty(),
        "a control exposed after pointer-down must not inherit the release click"
    );
    assert_eq!(doc.get_mousedown_node_id(), None);
}

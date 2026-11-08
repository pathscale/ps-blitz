use blitz_dom::{BaseDocument, Document, DocumentConfig, EventDriver, EventHandler, NodeId, qual_name};
use blitz_traits::events::{
    BlitzPointerEvent, DomEvent, DomEventData, EventState, MouseEventButtons,
};

fn checkbox_or_radio(
    doc: &mut BaseDocument,
    parent: NodeId,
    kind: &str,
    checked: bool,
) -> NodeId {
    let mut dom = doc.mutate();
    let input = dom.create_element(qual_name!("input", html), Vec::new());
    dom.set_attribute(input, qual_name!("type"), kind);
    if kind == "radio" {
        dom.set_attribute(input, qual_name!("name"), "choice");
    }
    if checked {
        dom.set_attribute(input, qual_name!("checked"), "");
    }
    dom.append_children(parent, &[input]);
    input
}

fn document() -> (BaseDocument, NodeId) {
    let mut doc = BaseDocument::new(DocumentConfig::default());
    let root = doc.root_node().id;
    let body = {
        let mut dom = doc.mutate();
        let html = dom.create_element(qual_name!("html", html), Vec::new());
        let body = dom.create_element(qual_name!("body", html), Vec::new());
        dom.append_children(root, &[html]);
        dom.append_children(html, &[body]);
        body
    };
    (doc, body)
}

fn checked(doc: &BaseDocument, node: NodeId) -> Option<bool> {
    doc.get_node(node)?.element_data()?.checkbox_input_checked()
}

type Observation = (&'static str, bool);

struct Recorder<'a> {
    input: NodeId,
    ancestor: NodeId,
    cancel_click: bool,
    seen: &'a mut Vec<Observation>,
}

impl EventHandler for Recorder<'_> {
    fn handle_event(
        &mut self,
        chain: &[NodeId],
        event: &mut DomEvent,
        doc: &mut dyn Document,
        state: &mut EventState,
    ) {
        if event.target != self.input {
            return;
        }
        match &event.data {
            DomEventData::Click(_) if self.cancel_click => state.prevent_default(),
            DomEventData::Click(_) | DomEventData::Input(_) => {}
            DomEventData::Change(_) => {
                assert!(event.bubbles);
                assert!(!event.cancelable);
                assert!(chain.contains(&self.ancestor));
            }
            _ => return,
        }
        self.seen
            .push((event.name(), checked(&doc.inner(), self.input).unwrap()));
    }
}

fn press(
    doc: &mut BaseDocument,
    target: NodeId,
    input: NodeId,
    ancestor: NodeId,
    cancel_click: bool,
) -> Vec<Observation> {
    let mut seen = Vec::new();
    EventDriver::new(
        doc,
        Recorder {
            input,
            ancestor,
            cancel_click,
            seen: &mut seen,
        },
    )
    .handle_dom_event(DomEvent::new(
        target,
        DomEventData::Click(BlitzPointerEvent::at(0.0, 0.0, MouseEventButtons::empty())),
    ));
    seen
}

#[test]
fn a_label_click_fires_one_input_then_change_on_its_checkbox() {
    let (mut doc, body) = document();
    let label = {
        let mut dom = doc.mutate();
        let label = dom.create_element(qual_name!("label", html), Vec::new());
        dom.append_children(body, &[label]);
        label
    };
    let input = checkbox_or_radio(&mut doc, label, "checkbox", false);
    doc.resolve(0.0);

    assert_eq!(
        press(&mut doc, label, input, label, false),
        [("click", true), ("input", true), ("change", true)]
    );
    assert_eq!(
        press(&mut doc, input, input, label, false),
        [("click", false), ("input", false), ("change", false)]
    );
}

#[test]
fn a_canceled_click_rolls_back_without_input_or_change() {
    let (mut doc, body) = document();
    let input = checkbox_or_radio(&mut doc, body, "checkbox", false);
    doc.resolve(0.0);

    assert_eq!(
        press(&mut doc, input, input, body, true),
        [("click", true)]
    );
    assert_eq!(checked(&doc, input), Some(false));
}

#[test]
fn only_a_newly_selected_radio_notifies() {
    let (mut doc, body) = document();
    let first = checkbox_or_radio(&mut doc, body, "radio", true);
    let second = checkbox_or_radio(&mut doc, body, "radio", false);
    doc.resolve(0.0);

    assert_eq!(
        press(&mut doc, second, second, body, false),
        [("click", true), ("input", true), ("change", true)]
    );
    assert_eq!(checked(&doc, first), Some(false));
    assert_eq!(
        press(&mut doc, second, second, body, false),
        [("click", true)]
    );
}

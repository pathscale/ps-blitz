use super::*;
use blitz_traits::events::{BlitzPointerEvent, DomEvent, DomEventData, MouseEventButtons, UiEvent};

const MAX_EVENT_PATH: usize = 4096;
const MAX_EVENT_CALLS: usize = 100_000;
const MAX_RESOURCE_EVENTS: usize = 4096;

impl<B: RuntimeBridge> Guest<B> {
    pub(super) fn install_events(&mut self) {
        let event = Value::native("Event");
        let event_prototype = Value::object("EventPrototype");
        event.put("prototype", event_prototype.clone());
        self.global.put("Event", event);
        let custom = Value::native("CustomEvent");
        let custom_prototype = Value::object("CustomEventPrototype");
        custom_prototype.as_object().unwrap().borrow_mut().prototype = Some(event_prototype);
        custom.put("prototype", custom_prototype);
        self.global.put("CustomEvent", custom);
        for name in ["UIEvent", "MouseEvent", "KeyboardEvent"] {
            let constructor = Value::native(name);
            let prototype = Value::object(format!("{name}Prototype"));
            prototype.as_object().unwrap().borrow_mut().prototype =
                Some(self.global.get("Event").get("prototype"));
            constructor.put("prototype", prototype);
            self.global.put(name, constructor);
        }
        let target = self.global.get("EventTarget");
        if target.kind() != "Native" {
            let target = Value::native("EventTarget");
            target.put("prototype", Value::object("EventTargetPrototype"));
            self.global.put("EventTarget", target);
        }
    }

    pub(super) fn events_get(
        &mut self,
        _doc: &mut BaseDocument,
        receiver: &Value,
        key: &str,
    ) -> Result<Option<Value>> {
        let kind = receiver.kind();
        if kind == "Event" || kind == "CustomEvent" {
            return Ok(match key {
                "preventDefault" => Some(Value::native("Event.preventDefault")),
                "stopPropagation" => Some(Value::native("Event.stopPropagation")),
                "stopImmediatePropagation" => Some(Value::native("Event.stopImmediatePropagation")),
                "composedPath" => Some(Value::native("Event.composedPath")),
                "initEvent" => Some(Value::native("Event.initEvent")),
                "initMouseEvent" if receiver.get("__interface").text() == "MouseEvent" => {
                    Some(Value::native("Event.initMouseEvent"))
                }
                "initCustomEvent" if receiver.get("__interface").text() == "CustomEvent" => {
                    Some(Value::native("Event.initCustomEvent"))
                }
                _ => None,
            });
        }
        if is_event_target(receiver) {
            return Ok(match key {
                "addEventListener" => Some(Value::native("EventTarget.addEventListener")),
                "removeEventListener" => Some(Value::native("EventTarget.removeEventListener")),
                "dispatchEvent" => Some(Value::native("EventTarget.dispatchEvent")),
                _ => None,
            });
        }
        Ok(None)
    }

    pub(super) fn events_set(
        &mut self,
        _doc: &mut BaseDocument,
        receiver: &Value,
        key: &str,
        value: Value,
    ) -> Result<bool> {
        if key.len() > 2 && key.starts_with("on") && is_event_target(receiver) {
            let event_name = key[2..].to_ascii_lowercase();
            let order = self.next_callback;
            self.next_callback = self.next_callback.wrapping_add(1).max(1);
            let orders = receiver.get("__event_on_order");
            if orders.kind() != "Object" {
                receiver.put_hidden("__event_on_order", Value::object("Object"));
            }
            receiver
                .get("__event_on_order")
                .put(&event_name, Value::number(order as f64));
            receiver.put(key, value);
            return Ok(true);
        }
        Ok(false)
    }

    pub(super) fn events_call(
        &mut self,
        doc: &mut BaseDocument,
        name: &str,
        this: Value,
        args: Vec<Value>,
        construct: bool,
    ) -> Result<Option<Value>> {
        let arg = |index: usize| args.get(index).cloned().unwrap_or(Value::Undefined);
        let result = match name {
            "Event" | "CustomEvent" | "UIEvent" | "MouseEvent" | "KeyboardEvent" => {
                if !construct {
                    return Err(Error::type_error("Event constructor requires new"));
                }
                let event_type = arg(0).text();
                if event_type.is_empty() {
                    return Err(Error::new("TypeError", "event type must not be empty"));
                }
                let options = arg(1);
                let event = new_event(&event_type, &options, name == "CustomEvent");
                event.put_hidden("__interface", Value::string(name));
                for key in [
                    "detail", "screenX", "screenY", "clientX", "clientY", "button", "buttons",
                    "ctrlKey", "altKey", "shiftKey", "metaKey", "key", "code", "keyCode",
                    "charCode", "view",
                ] {
                    let value = options.get(key);
                    if !matches!(value, Value::Undefined) {
                        event.put(key, value);
                    }
                }
                let prototype_name = if name == "CustomEvent"
                    || name == "UIEvent"
                    || name == "MouseEvent"
                    || name == "KeyboardEvent"
                {
                    name
                } else {
                    "Event"
                };
                event.as_object().unwrap().borrow_mut().prototype =
                    Some(self.global.get(prototype_name).get("prototype"));
                Some(event)
            }
            "EventTarget" => {
                if !construct {
                    return Err(Error::type_error("EventTarget constructor requires new"));
                }
                let target = Value::object("EventTarget");
                target.as_object().unwrap().borrow_mut().prototype =
                    Some(self.global.get("EventTarget").get("prototype"));
                Some(target)
            }
            "EventTarget.addEventListener" | "EventTarget.removeEventListener" => {
                let event_type = arg(0).text();
                let callback = arg(1);
                let options = arg(2);
                if !event_type.is_empty() && is_callable(&callback) {
                    let (capture, once) = listener_options(&options);
                    let mut listeners = listener_records(&this);
                    if name.ends_with("addEventListener") {
                        let duplicate = listeners.iter().any(|record| {
                            record.get("type").text() == event_type
                                && record.get("capture").truthy() == capture
                                && record.get("callback").same(&callback)
                        });
                        if !duplicate {
                            let record = Value::object("EventListenerRecord");
                            record.put("type", Value::string(event_type));
                            record.put("callback", callback);
                            record.put("capture", Value::Bool(capture));
                            record.put("once", Value::Bool(once));
                            record.put("order", Value::number(self.next_callback as f64));
                            self.next_callback = self.next_callback.wrapping_add(1).max(1);
                            listeners.push(record);
                            this.put_hidden("__listeners", Value::array(listeners));
                        }
                    } else {
                        let mut removed = false;
                        let kept = listeners
                            .into_iter()
                            .filter(|record| {
                                if !removed
                                    && record.get("type").text() == event_type
                                    && record.get("capture").truthy() == capture
                                    && record.get("callback").same(&callback)
                                {
                                    removed = true;
                                    false
                                } else {
                                    true
                                }
                            })
                            .collect();
                        this.put_hidden("__listeners", Value::array(kept));
                    }
                }
                Some(Value::Undefined)
            }
            "EventTarget.dispatchEvent" => {
                Some(Value::Bool(self.dispatch_event(doc, this, arg(0))?))
            }
            "Event.preventDefault" => {
                if this.get("cancelable").truthy() && !this.get("__in_passive_listener").truthy() {
                    this.put("defaultPrevented", Value::Bool(true));
                    this.put_hidden("__canceled", Value::Bool(true));
                }
                Some(Value::Undefined)
            }
            "Event.stopPropagation" => {
                this.put_hidden("__stopped", Value::Bool(true));
                Some(Value::Undefined)
            }
            "Event.stopImmediatePropagation" => {
                this.put_hidden("__stopped", Value::Bool(true));
                this.put_hidden("__immediate", Value::Bool(true));
                Some(Value::Undefined)
            }
            "Event.composedPath" => Some(Value::array(this.get("__path").items())),
            "Event.initEvent" => {
                if !this.get("__dispatching").truthy() {
                    this.put("type", arg(0));
                    this.put("bubbles", Value::Bool(arg(1).truthy()));
                    this.put("cancelable", Value::Bool(arg(2).truthy()));
                    this.put("defaultPrevented", Value::Bool(false));
                }
                Some(Value::Undefined)
            }
            "Event.initMouseEvent" => {
                if !this.get("__dispatching").truthy() {
                    this.put("type", arg(0));
                    this.put("bubbles", Value::Bool(arg(1).truthy()));
                    this.put("cancelable", Value::Bool(arg(2).truthy()));
                    for (key, index) in [
                        ("view", 3),
                        ("detail", 4),
                        ("screenX", 5),
                        ("screenY", 6),
                        ("clientX", 7),
                        ("clientY", 8),
                        ("ctrlKey", 9),
                        ("altKey", 10),
                        ("shiftKey", 11),
                        ("metaKey", 12),
                        ("button", 13),
                        ("relatedTarget", 14),
                    ] {
                        this.put(key, arg(index));
                    }
                }
                Some(Value::Undefined)
            }
            "Event.initCustomEvent" => {
                if !this.get("__dispatching").truthy() {
                    this.put("type", arg(0));
                    this.put("bubbles", Value::Bool(arg(1).truthy()));
                    this.put("cancelable", Value::Bool(arg(2).truthy()));
                    this.put("detail", arg(3));
                }
                Some(Value::Undefined)
            }
            _ => None,
        };
        Ok(result)
    }

    pub(super) fn dispatch_event(
        &mut self,
        doc: &mut BaseDocument,
        target: Value,
        event: Value,
    ) -> Result<bool> {
        if !is_event_target(&target) {
            return Err(Error::type_error("dispatch target is not an EventTarget"));
        }
        if !matches!(event.kind().as_str(), "Event" | "CustomEvent") {
            return Err(Error::type_error("dispatchEvent requires an Event"));
        }
        if event
            .as_object()
            .is_some_and(|object| object.borrow().prototype.is_none())
        {
            let prototype = self
                .global
                .get(if event.kind() == "CustomEvent" {
                    "CustomEvent"
                } else {
                    "Event"
                })
                .get("prototype");
            event.as_object().unwrap().borrow_mut().prototype = Some(prototype);
        }
        if event.get("__dispatching").truthy() {
            return Err(Error::new(
                "InvalidStateError",
                "event is already being dispatched",
            ));
        }
        let event_type = event.get("type").text();
        if event_type.is_empty() {
            return Err(Error::type_error("event type must not be empty"));
        }
        let target_node = target.node_id();
        let mut path = target_node
            .map(|id| {
                let mut ids = doc.node_chain(id);
                if !event.get("composed").truthy() {
                    if let Some(shadow_index) = ids.iter().position(|candidate| {
                        doc.get_node(*candidate)
                            .is_some_and(|node| node.is_shadow_root())
                    }) {
                        ids.truncate(shadow_index + 1);
                    }
                }
                ids.into_iter().map(|id| self.typed_node(doc, id)).collect()
            })
            .unwrap_or_else(|| vec![target.clone()]);
        if path.len() > MAX_EVENT_PATH {
            return Err(Error::new(
                "RangeError",
                "event propagation path limit exceeded",
            ));
        }
        if path.first().is_none_or(|item| !item.same(&target)) {
            path.insert(0, target.clone());
        }
        event.put("target", target.clone());
        event.put("currentTarget", Value::Null);
        event.put("eventPhase", Value::number(0));
        event.put_hidden("__path", Value::array(path.clone()));
        event.put_hidden("__dispatching", Value::Bool(true));
        event.put_hidden("__stopped", Value::Bool(false));
        event.put_hidden("__immediate", Value::Bool(false));

        let mut calls = 0usize;
        let ancestors = &path[1..];
        for current in ancestors.iter().rev() {
            if event.get("__stopped").truthy() {
                break;
            }
            self.dispatch_at(doc, current, &event, &event_type, (true, 1), &mut calls)?;
        }
        if !event.get("__stopped").truthy() {
            self.dispatch_at(doc, &target, &event, &event_type, (true, 2), &mut calls)?;
            if !event.get("__immediate").truthy() {
                self.dispatch_at(doc, &target, &event, &event_type, (false, 2), &mut calls)?;
            }
        }
        if event.get("bubbles").truthy() && !event.get("__stopped").truthy() {
            for current in ancestors {
                if event.get("__stopped").truthy() {
                    break;
                }
                self.dispatch_at(doc, current, &event, &event_type, (false, 3), &mut calls)?;
            }
        }
        event.put("currentTarget", Value::Null);
        event.put("eventPhase", Value::number(0));
        event.put_hidden("__dispatching", Value::Bool(false));
        if let Some(node) = target_node.filter(|_| !event.get("\0native_default").truthy()) {
            self.forward_default(doc, node, &event)?;
        }
        Ok(!event.get("defaultPrevented").truthy())
    }

    fn dispatch_at(
        &mut self,
        doc: &mut BaseDocument,
        current: &Value,
        event: &Value,
        event_type: &str,
        phase: (bool, u8),
        calls: &mut usize,
    ) -> Result<()> {
        let (capture, phase) = phase;
        event.put("currentTarget", current.clone());
        event.put("eventPhase", Value::number(phase as f64));
        event.put_hidden("__immediate", Value::Bool(false));
        let listeners = listener_records(current);
        let on_key = format!("on{event_type}");
        let on_handler = self.get_property(doc, current, &on_key)?;
        let on_order = current
            .get("__event_on_order")
            .get(event_type)
            .number_value() as u64;
        let mut entries: Vec<(u64, Value, bool, bool, bool)> = listeners
            .into_iter()
            .filter(|record| record.get("type").text() == event_type)
            .map(|record| {
                (
                    record.get("order").number_value() as u64,
                    record.get("callback"),
                    record.get("capture").truthy(),
                    record.get("once").truthy(),
                    false,
                )
            })
            .collect();
        if !on_handler.same(&Value::Undefined) && is_callable(&on_handler) {
            entries.push((on_order, on_handler, false, false, true));
        }
        if event.get("\0resource_event").truthy() {
            entries.sort_by_key(|entry| (!entry.4, entry.0));
        } else {
            entries.sort_by_key(|entry| entry.0);
        }
        for (order, callback, is_capture, once, legacy_handler) in entries {
            if is_capture != capture {
                continue;
            }
            if *calls >= MAX_EVENT_CALLS {
                return Err(Error::new(
                    "RangeError",
                    "event listener call limit exceeded",
                ));
            }
            *calls += 1;
            if once {
                let retained = listener_records(current)
                    .into_iter()
                    .filter(|record| {
                        !(record.get("type").text() == event_type
                            && record.get("order").number_value() as u64 == order)
                    })
                    .collect();
                current.put_hidden("__listeners", Value::array(retained));
            }
            match self.call(
                doc,
                callback.clone(),
                current.clone(),
                vec![event.clone()],
                false,
            ) {
                Ok(Value::Bool(false)) if legacy_handler && event.get("cancelable").truthy() => {
                    event.put_hidden("__canceled", Value::Bool(true));
                    event.put("defaultPrevented", Value::Bool(true));
                }
                Ok(_) => {}
                Err(error) => self.report_error(error),
            }
            if event.get("__immediate").truthy() {
                break;
            }
        }
        Ok(())
    }

    fn forward_default(
        &mut self,
        doc: &mut BaseDocument,
        node: NodeId,
        event: &Value,
    ) -> Result<()> {
        if event.get("defaultPrevented").truthy() || !event.get("cancelable").truthy() {
            return Ok(());
        }
        let Some(dom_event) = default_dom_event(node, &event.get("type").text(), event) else {
            return Ok(());
        };
        let mut generated = Vec::new();
        doc.handle_dom_event(&mut dom_event.clone(), |next| generated.push(next));
        for next in generated {
            let target = self.typed_node(doc, next.target);
            let translated = event_from_dom(&next);
            self.dispatch_event(doc, target, translated)?;
        }
        Ok(())
    }

    pub(super) fn dispatch_input(
        &mut self,
        doc: &mut BaseDocument,
        event: UiEvent,
        node: Option<NodeId>,
    ) -> Result<()> {
        let mut driver = blitz_dom::EventDriver::new(doc, NativeEventHandler(self));
        if let Some(node) = node {
            driver.handle_ui_event_to_node(event, node);
        } else {
            driver.handle_ui_event(event);
        }
        Ok(())
    }

    pub(super) fn dispatch_native_event(
        &mut self,
        doc: &mut BaseDocument,
        event: DomEvent,
    ) -> Result<()> {
        blitz_dom::EventDriver::new(doc, NativeEventHandler(self)).handle_dom_event(event);
        Ok(())
    }

    /// Queue a resource completion for dispatch on the next guest poll.
    pub(super) fn queue_resource_event(
        &mut self,
        doc: &BaseDocument,
        node: NodeId,
        source: &str,
        event_type: &str,
    ) {
        let target = self.typed_node(doc, node);
        let event = Value::object("Event");
        event.put("type", Value::string(event_type));
        event.put("bubbles", Value::Bool(false));
        event.put("cancelable", Value::Bool(false));
        event.put_hidden("\0resource_event", Value::Bool(true));
        let record = Value::object("ResourceEvent");
        record.put("target", target);
        record.put("source", Value::string(source));
        record.put("event", event);
        let pending = self.global.get("\0resource_events");
        let mut events = pending.items();
        if events.len() >= MAX_RESOURCE_EVENTS {
            return;
        }
        events.push(record);
        self.global
            .put_hidden("\0resource_events", Value::array(events));
    }

    /// Dispatch resource completions after the script that initiated them returns.
    pub(super) fn dispatch_resource_events(&mut self, doc: &mut BaseDocument) -> Result<bool> {
        let pending = self.global.get("\0resource_events");
        let events = pending.items();
        if events.is_empty() {
            return Ok(false);
        }
        self.global
            .put_hidden("\0resource_events", Value::array(Vec::new()));
        for record in events {
            let target = record.get("target");
            let Some(node) = target.node_id() else {
                continue;
            };
            let Some(element) = doc.get_node(node).and_then(|node| node.element_data()) else {
                continue;
            };
            if element.name.local.as_ref() != "img"
                || element.attr(blitz_dom::local_name!("src")).unwrap_or("")
                    != record.get("source").text()
            {
                continue;
            }
            self.dispatch_event(doc, target, record.get("event"))?;
        }
        Ok(true)
    }

    pub(super) fn drain_jobs(&mut self, doc: &mut BaseDocument, limit: usize) -> Result<bool> {
        let bound = limit.min(MAX_EVENT_CALLS);
        for _ in 0..bound {
            let Some(job) = self.jobs.pop_front() else {
                return Ok(false);
            };
            match self.call(doc, job.callback, job.this, job.args, false) {
                Ok(_) => {}
                Err(error) => self.report_error(error),
            }
        }
        Ok(!self.jobs.is_empty())
    }
}

fn is_callable(value: &Value) -> bool {
    matches!(value.kind().as_str(), "Function" | "Native")
}

fn is_event_target(value: &Value) -> bool {
    value.as_object().is_some()
}

fn listener_records(target: &Value) -> Vec<Value> {
    target.get("__listeners").items()
}

fn listener_options(options: &Value) -> (bool, bool) {
    if options.kind() == "Object" {
        (
            options.get("capture").truthy(),
            options.get("once").truthy(),
        )
    } else {
        (options.truthy(), false)
    }
}

fn new_event(event_type: &str, options: &Value, custom: bool) -> Value {
    let event = Value::object(if custom { "CustomEvent" } else { "Event" });
    event.put("type", Value::string(event_type));
    event.put("bubbles", Value::Bool(options.get("bubbles").truthy()));
    event.put(
        "cancelable",
        Value::Bool(options.get("cancelable").truthy()),
    );
    event.put("composed", Value::Bool(options.get("composed").truthy()));
    event.put("defaultPrevented", Value::Bool(false));
    event.put("target", Value::Null);
    event.put("currentTarget", Value::Null);
    event.put("eventPhase", Value::number(0));
    event.put("timeStamp", Value::number(0));
    if custom {
        event.put("detail", options.get("detail"));
    }
    event
}

fn default_dom_event(target: NodeId, name: &str, event: &Value) -> Option<DomEvent> {
    let pointer = || {
        let x = event.get("pageX").number_value();
        let y = event.get("pageY").number_value();
        let x = if x.is_finite() { x as f32 } else { 0.0 };
        let y = if y.is_finite() { y as f32 } else { 0.0 };
        let buttons =
            MouseEventButtons::from_bits_truncate(event.get("buttons").number_value() as u8);
        BlitzPointerEvent::at(x, y, buttons)
    };
    let data = match name {
        "click" => DomEventData::Click(pointer()),
        "dblclick" => DomEventData::DoubleClick(pointer()),
        "contextmenu" => DomEventData::ContextMenu(pointer()),
        _ => return None,
    };
    Some(DomEvent::new(target, data))
}

fn event_from_dom(event: &DomEvent) -> Value {
    let value = new_event(event.name(), &Value::Undefined, false);
    value.put("bubbles", Value::Bool(event.bubbles));
    value.put("cancelable", Value::Bool(event.cancelable));
    value
}

struct NativeEventHandler<'a, B: RuntimeBridge>(&'a mut Guest<B>);
impl<B: RuntimeBridge> blitz_dom::EventHandler for NativeEventHandler<'_, B> {
    fn handle_event(
        &mut self,
        _chain: &[NodeId],
        event: &mut DomEvent,
        document: &mut dyn blitz_dom::Document,
        state: &mut blitz_traits::events::EventState,
    ) {
        let mut document = document.inner_mut();
        let target = self.0.typed_node(&document, event.target);
        let value = event_from_dom(event);
        value.put_hidden("\0native_default", Value::Bool(true));
        match self.0.dispatch_event(&mut document, target, value) {
            Ok(false) => state.prevent_default(),
            Ok(true) => {}
            Err(error) => self.0.report_error(error),
        }
    }
}

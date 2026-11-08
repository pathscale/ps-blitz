use super::buffer::{ArrayBuffer, TypedArray};
use super::web::BlobBytes;
use super::*;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Weak;

const MAX_PENDING_MESSAGE_BYTES: usize = 64 * 1024 * 1024;
const MAX_MESSAGE_NESTING: usize = 128;

struct PortMessage {
    data: Value,
    ports: Vec<Value>,
    snapshot: PendingSnapshot,
}

struct PendingSnapshot {
    total: Rc<Cell<usize>>,
    bytes: usize,
    objects: Vec<Weak<RefCell<Object>>>,
}

impl PendingSnapshot {
    fn charge(&mut self, bytes: usize) -> Result<()> {
        let next = self.total.get().saturating_add(bytes);
        if next > MAX_PENDING_MESSAGE_BYTES {
            return Err(Error::new(
                "QuotaExceededError",
                "MessagePort snapshots exceed size limit",
            ));
        }
        self.total.set(next);
        self.bytes += bytes;
        Ok(())
    }

    fn publish(mut self) {
        self.objects.clear();
    }
}

impl Drop for PendingSnapshot {
    fn drop(&mut self) {
        // Until delivery, only this message can expose the snapshot. Break its
        // graph edges on failure or discard so Rc cycles cannot retain the data.
        for object in self.objects.iter().filter_map(Weak::upgrade) {
            if let Ok(mut object) = object.try_borrow_mut() {
                object.props.clear();
                object.indexed.clear();
            }
        }
        self.total.set(self.total.get() - self.bytes);
    }
}

#[derive(Default)]
struct PortState {
    owner: Weak<RefCell<Object>>,
    // Entanglement retains the remote wrapper, including its listeners. Close
    // breaks the link, and realm teardown releases otherwise unreachable pairs.
    peer: Option<Value>,
    inbox: VecDeque<PortMessage>,
    started: bool,
    scheduled: bool,
}

fn port_state(port: &Value) -> Option<Rc<RefCell<PortState>>> {
    port.as_object()?.borrow().payload.clone()?.downcast().ok()
}

fn attach_port(port: &Value, state: &Rc<RefCell<PortState>>) {
    let object = port.as_object().unwrap();
    {
        let mut state = state.borrow_mut();
        state.owner = Rc::downgrade(&object);
        state.started = false;
        state.scheduled = false;
    }
    object.borrow_mut().payload = Some(state.clone());
}

pub(super) fn message_channel() -> Value {
    let port1 = Value::object("MessagePort");
    let port2 = Value::object("MessagePort");
    for (port, peer) in [(&port1, &port2), (&port2, &port1)] {
        let state = Rc::new(RefCell::new(PortState {
            peer: Some(peer.clone()),
            ..PortState::default()
        }));
        attach_port(port, &state);
    }
    let channel = Value::object("MessageChannel");
    channel.put("port1", port1);
    channel.put("port2", port2);
    channel
}

fn discard_message_ports(mut pending: Vec<Value>) {
    while let Some(port) = pending.pop() {
        let Some(state) = port_state(&port) else {
            continue;
        };
        port.as_object().unwrap().borrow_mut().payload = None;
        let (peer, inbox) = {
            let mut state = state.borrow_mut();
            state.owner = Weak::new();
            (state.peer.take(), std::mem::take(&mut state.inbox))
        };
        if let Some(peer) = peer.as_ref().and_then(port_state) {
            peer.borrow_mut().peer = None;
        }
        // These wrappers were moved into messages that can no longer be delivered.
        for message in inbox {
            pending.extend(message.ports);
        }
    }
}

enum Transfer {
    Buffer(Rc<ArrayBuffer>, Rc<ArrayBuffer>),
    Port(Value, Value),
}

struct MessageClone {
    // Keep source objects alive while getters can create further objects.
    seen: HashMap<*const RefCell<Object>, (Value, Value)>,
    pending: PendingSnapshot,
}

impl MessageClone {
    fn charge(&mut self, bytes: usize) -> Result<()> {
        self.pending.charge(bytes)
    }
}

impl<B: RuntimeBridge> Guest<B> {
    fn check_message_port(&self, port: &Value) -> Result<()> {
        if port.kind() != "MessagePort" {
            return Err(Error::type_error("receiver is not a MessagePort"));
        }
        if port
            .as_object()
            .and_then(|object| object.borrow().realm)
            .is_some_and(|realm| realm != self.scope_id)
        {
            return Err(Error::new(
                "InvalidStateError",
                "MessagePort belongs to another realm",
            ));
        }
        Ok(())
    }

    pub(super) fn set_message_port_handler(
        &mut self,
        doc: &mut BaseDocument,
        port: &Value,
        handler: Value,
    ) -> Result<()> {
        self.check_message_port(port)?;
        self.events_set(doc, port, "onmessage", handler)?;
        self.enable_message_port(port);
        Ok(())
    }

    pub(super) fn start_message_port(&mut self, port: &Value) -> Result<()> {
        self.check_message_port(port)?;
        self.enable_message_port(port);
        Ok(())
    }

    pub(super) fn close_message_port(&self, port: &Value) -> Result<()> {
        self.check_message_port(port)?;
        discard_message_ports(vec![port.clone()]);
        Ok(())
    }

    fn enable_message_port(&mut self, port: &Value) {
        if let Some(state) = port_state(port) {
            state.borrow_mut().started = true;
            self.schedule_message_port(port, &state);
        }
    }

    fn schedule_message_port(&mut self, port: &Value, state: &RefCell<PortState>) {
        let mut state = state.borrow_mut();
        if state.started && !state.scheduled && !state.inbox.is_empty() {
            state.scheduled = true;
            self.queue(
                Value::native("Guest.deliverMessagePort"),
                port.clone(),
                Vec::new(),
            );
        }
    }

    pub(super) fn deliver_message_port(
        &mut self,
        doc: &mut BaseDocument,
        port: Value,
    ) -> Result<()> {
        self.check_message_port(&port)?;
        let Some(state) = port_state(&port) else {
            // A queued job for a wrapper that was closed or transferred is stale.
            return Ok(());
        };
        let message = {
            let mut state = state.borrow_mut();
            state.scheduled = false;
            state.inbox.pop_front()
        };
        if let Some(PortMessage {
            data,
            ports,
            snapshot,
        }) = message
        {
            snapshot.publish();
            self.schedule_message_port(&port, &state);
            let event = Value::object("MessageEvent");
            event.put("type", Value::string("message"));
            event.put("data", data);
            let ports = Value::array(ports);
            ports.as_object().unwrap().borrow_mut().frozen = true;
            event.put("ports", ports);
            self.dispatch_event(doc, port, event)?;
        }
        Ok(())
    }

    pub(super) fn post_message_port(
        &mut self,
        doc: &mut BaseDocument,
        source: &Value,
        data: &Value,
        options: &Value,
    ) -> Result<()> {
        if self.message_clone_depth >= MAX_MESSAGE_NESTING {
            return Err(self.data_clone_error("message nesting limit exceeded"));
        }
        self.message_clone_depth += 1;
        let result = self.serialize_port_message(doc, source, data, options);
        self.message_clone_depth -= 1;
        result
    }

    fn serialize_port_message(
        &mut self,
        doc: &mut BaseDocument,
        source: &Value,
        data: &Value,
        options: &Value,
    ) -> Result<()> {
        self.check_message_port(source)?;
        let transfer_list = self.message_transfer_list(doc, options)?;
        let peer = port_state(source).and_then(|state| state.borrow().peer.clone());
        let destination = peer.as_ref().and_then(port_state);
        let mut doomed = false;
        let mut snapshot = MessageClone {
            seen: HashMap::new(),
            pending: PendingSnapshot {
                total: self.pending_message_bytes.clone(),
                bytes: 0,
                objects: Vec::new(),
            },
        };
        snapshot.charge(std::mem::size_of::<PortMessage>())?;
        let mut transfers = Vec::with_capacity(transfer_list.len());
        for value in transfer_list {
            self.tick()?;
            let object = value.as_object().ok_or_else(|| {
                Error::type_error("MessagePort transfer list entries must be objects")
            })?;
            let id = Rc::as_ptr(&object);
            if object
                .borrow()
                .realm
                .is_some_and(|realm| realm != self.scope_id)
            {
                return Err(self.data_clone_error("transferable belongs to another realm"));
            }
            if value.same(source) || snapshot.seen.contains_key(&id) {
                return Err(
                    self.data_clone_error("duplicate or source MessagePort in transfer list")
                );
            }

            snapshot.charge(std::mem::size_of::<Object>())?;
            let (copy, transfer) = if let Some(buffer) = ArrayBuffer::of(&value) {
                if buffer.len().is_none() {
                    return Err(self.data_clone_error("transferable is detached"));
                }
                let copy = ArrayBuffer::new_value(None);
                let target = ArrayBuffer::of(&copy).unwrap();
                (copy, Transfer::Buffer(buffer, target))
            } else if port_state(&value).is_some() {
                doomed |= peer.as_ref().is_some_and(|peer| peer.same(&value));
                let copy = Value::object("MessagePort");
                (copy.clone(), Transfer::Port(value.clone(), copy))
            } else {
                return Err(self.data_clone_error("object is not transferable"));
            };
            snapshot.seen.insert(id, (value, copy));
            transfers.push(transfer);
        }
        let data = self.clone_message_value(doc, data, &mut snapshot, 0)?;

        // Getters can run during cloning, including another postMessage. Validate
        // all transfers again before changing any ownership.
        for transfer in &transfers {
            match transfer {
                Transfer::Buffer(buffer, _) => {
                    let length = buffer
                        .len()
                        .ok_or_else(|| self.data_clone_error("transferable is detached"))?;
                    snapshot.charge(length)?;
                }
                Transfer::Port(from, _) if port_state(from).is_none() => {
                    return Err(self.data_clone_error("transferable is detached"));
                }
                Transfer::Port(..) => {}
            }
        }
        // A reentrant getter may have closed or moved the receiving wrapper.
        let receiver = destination.filter(|_| !doomed).and_then(|state| {
            let owner = state.borrow().owner.upgrade()?;
            Some((state, Value::Object(owner)))
        });
        let mut ports = Vec::new();
        for transfer in transfers {
            match transfer {
                Transfer::Buffer(from, to) => from.transfer_to(&to),
                Transfer::Port(from, to) => {
                    let state = port_state(&from).unwrap();
                    from.as_object().unwrap().borrow_mut().payload = None;
                    attach_port(&to, &state);
                    let peer = state.borrow().peer.as_ref().and_then(port_state);
                    if let Some(peer) = peer {
                        peer.borrow_mut().peer = Some(to.clone());
                    }
                    ports.push(to);
                }
            }
        }

        if let Some((destination, owner)) = receiver {
            destination.borrow_mut().inbox.push_back(PortMessage {
                data,
                ports,
                snapshot: snapshot.pending,
            });
            self.schedule_message_port(&owner, &destination);
            return Ok(());
        }

        discard_message_ports(ports);
        Ok(())
    }

    fn message_transfer_list(
        &mut self,
        doc: &mut BaseDocument,
        options: &Value,
    ) -> Result<Vec<Value>> {
        match options {
            Value::Undefined | Value::Null => Ok(Vec::new()),
            Value::Object(_) => {
                let iterator = self.get_property(doc, options, "@@iterator")?;
                if !matches!(iterator, Value::Undefined | Value::Null) {
                    if !super::proxy::is_callable(&iterator) {
                        return Err(Error::type_error(
                            "MessagePort transfer iterator must be callable",
                        ));
                    }
                    return self.collect_iterator(doc, options, iterator);
                }
                let list = self.get_property(doc, options, "transfer")?;
                if matches!(list, Value::Undefined) {
                    return Ok(Vec::new());
                }
                if list.as_object().is_none() {
                    return Err(Error::type_error(
                        "MessagePort transfer list must be iterable",
                    ));
                }
                let iterator = self.get_property(doc, &list, "@@iterator")?;
                if !super::proxy::is_callable(&iterator) {
                    return Err(Error::type_error(
                        "MessagePort transfer list must be iterable",
                    ));
                }
                self.collect_iterator(doc, &list, iterator)
            }
            _ => Err(Error::type_error("invalid MessagePort postMessage options")),
        }
    }

    fn data_clone_error(&self, message: &str) -> Error {
        let exception = Value::object("DOMException");
        exception.as_object().unwrap().borrow_mut().prototype =
            Some(self.global.get("DOMException").get("prototype"));
        exception.put("name", Value::string("DataCloneError"));
        exception.put("message", Value::string(message));
        exception.put("code", Value::Number(25.0));
        Error::thrown(exception)
    }

    fn charge_message_bytes(&mut self, bytes: usize) -> Result<()> {
        // Charge bulk copies as well as graph edges against the guest's execution budget.
        self.steps = self.steps.saturating_add(bytes.div_ceil(16));
        self.tick()
    }

    fn clone_message_property(
        &mut self,
        doc: &mut BaseDocument,
        source: &Value,
        property: Property,
        snapshot: &mut MessageClone,
        depth: usize,
    ) -> Result<Value> {
        let value = if let Some(getter) = property.get {
            // An accessor can synchronously post another graph.
            self.message_clone_depth += depth + 1;
            let result = self.call(doc, getter, source.clone(), Vec::new(), false);
            self.message_clone_depth -= depth + 1;
            result?
        } else {
            property.value
        };
        self.clone_message_value(doc, &value, snapshot, depth + 1)
    }

    fn clone_message_value(
        &mut self,
        doc: &mut BaseDocument,
        value: &Value,
        snapshot: &mut MessageClone,
        depth: usize,
    ) -> Result<Value> {
        self.tick()?;
        let Value::Object(object) = value else {
            if let Value::String(units) = value {
                snapshot.charge(std::mem::size_of_val(units.as_ref()))?;
            }
            return Ok(value.clone());
        };
        let id = Rc::as_ptr(object);
        if let Some((_, copy)) = snapshot.seen.get(&id) {
            return Ok(copy.clone());
        }
        if self.message_clone_depth + depth >= MAX_MESSAGE_NESTING {
            return Err(self.data_clone_error("message nesting limit exceeded"));
        }
        let (kind, payload) = {
            let object = object.borrow();
            if object.function.is_some()
                || object.native.is_some()
                || object.realm.is_some_and(|realm| realm != self.scope_id)
            {
                return Err(self.data_clone_error("object cannot be cloned"));
            }
            (object.kind.clone(), object.payload.clone())
        };

        snapshot.charge(std::mem::size_of::<Object>())?;
        let copy = if let Some(buffer) = ArrayBuffer::of(value) {
            let len = buffer
                .len()
                .ok_or_else(|| self.data_clone_error("ArrayBuffer is detached"))?;
            self.charge_message_bytes(len)?;
            snapshot.charge(len)?;
            ArrayBuffer::new_value(buffer.bytes())
        } else if let Some(view) = TypedArray::of(value) {
            if view.len().is_none() {
                return Err(self.data_clone_error("typed array has a detached buffer"));
            }
            let buffer = self.clone_message_value(doc, &view.buffer, snapshot, depth + 1)?;
            TypedArray::new_value(&kind, buffer, view.offset, view.count)
        } else {
            match (kind.as_str(), payload.as_deref()) {
                ("Object" | "Array" | "Map" | "Set" | "Error" | "DOMException", None) => {
                    Value::object(&kind)
                }
                ("Blob", Some(bytes)) if bytes.is::<BlobBytes>() => Value::object(&kind),
                ("RegExp", Some(regex)) if regex.is::<regex::Regex>() => Value::object(&kind),
                _ => return Err(self.data_clone_error("object cannot be cloned")),
            }
        };
        snapshot.seen.insert(id, (value.clone(), copy.clone()));

        let copy_object = copy.as_object().unwrap();
        if matches!(kind.as_str(), "Object" | "Array" | "Map" | "Set" | "Error" | "DOMException") {
            snapshot.pending.objects.push(Rc::downgrade(&copy_object));
        }
        if matches!(kind.as_str(), "Object" | "Array") {
            copy_object.borrow_mut().prototype = Some(self.global.get(&kind).get("prototype"));
        }
        match kind.as_str() {
            "Object" | "Array" | "Map" | "Set" => {
                let (length, keys, index_properties) = {
                    let object = object.borrow();
                    let length = object.indexed.len();
                    snapshot.charge(length.saturating_mul(std::mem::size_of::<Value>()))?;
                    let mut keys = Vec::new();
                    let mut index_properties = HashMap::new();
                    if matches!(kind.as_str(), "Object" | "Array") {
                        for (key, property) in &object.props {
                            if kind == "Array"
                                && let Ok(index) = key.parse::<usize>()
                                && index < length
                                && index.to_string() == *key
                            {
                                index_properties.insert(index, (key.clone(), property.enumerable));
                            } else if property.enumerable
                                && !super::exec::is_symbol_property_key(&self.global, key)
                            {
                                snapshot.charge(
                                    key.len().saturating_add(std::mem::size_of::<Property>()),
                                )?;
                                keys.push(key.clone());
                            }
                        }
                    }
                    (length, keys, index_properties)
                };
                let items = if kind == "Array" {
                    let mut items = Vec::with_capacity(length);
                    for index in 0..length {
                        let copy = match index_properties.get(&index) {
                            Some((key, true)) => {
                                let property = object
                                    .borrow()
                                    .props
                                    .get(key)
                                    .filter(|property| property.enumerable)
                                    .cloned();
                                property
                                    .map(|property| {
                                        self.clone_message_property(
                                            doc, value, property, snapshot, depth,
                                        )
                                    })
                                    .transpose()?
                                    .unwrap_or_default()
                            }
                            Some((_, false)) => {
                                self.tick()?;
                                Value::Undefined
                            }
                            None => {
                                let item = object
                                    .borrow()
                                    .indexed
                                    .get(index)
                                    .cloned()
                                    .unwrap_or_default();
                                self.clone_message_value(doc, &item, snapshot, depth + 1)?
                            }
                        };
                        items.push(copy);
                    }
                    items
                } else {
                    let items = object.borrow().indexed.clone();
                    items
                        .iter()
                        .map(|value| self.clone_message_value(doc, value, snapshot, depth + 1))
                        .collect::<Result<Vec<_>>>()?
                };
                copy_object.borrow_mut().indexed = items;
                for key in keys {
                    let property = object
                        .borrow()
                        .props
                        .get(&key)
                        .filter(|property| property.enumerable)
                        .cloned();
                    if let Some(property) = property {
                        let value =
                            self.clone_message_property(doc, value, property, snapshot, depth)?;
                        copy_object
                            .borrow_mut()
                            .props
                            .insert(key, Property::data(value));
                    }
                }
            }
            "Error" | "DOMException" => {
                for key in ["name", "message", "cause", "code", "\0error_stack"] {
                    let value = value.get(key);
                    if !matches!(value, Value::Undefined) {
                        let value = self.clone_message_value(doc, &value, snapshot, depth + 1)?;
                        if key.starts_with('\0') {
                            copy.put_hidden(key, value);
                        } else {
                            copy.put(key, value);
                        }
                    }
                }
                let name = if kind == "DOMException" {
                    "DOMException"
                } else {
                    let name = match copy.get("name") {
                        Value::String(units) => String::from_utf16_lossy(&units),
                        _ => String::new(),
                    };
                    match name.as_str() {
                        "TypeError" => "TypeError",
                        "RangeError" => "RangeError",
                        "ReferenceError" => "ReferenceError",
                        "SyntaxError" => "SyntaxError",
                        _ => "Error",
                    }
                };
                copy_object.borrow_mut().prototype = Some(self.global.get(name).get("prototype"));
            }
            "Blob" => {
                let bytes = payload
                    .as_deref()
                    .unwrap()
                    .downcast_ref::<BlobBytes>()
                    .unwrap();
                copy.put("size", Value::Number(bytes.0.len() as f64));
                if let Value::String(mime) = value.get("type") {
                    copy.put("type", Value::String(mime));
                }
                // Blob payloads are immutable. Value::items can read the payload
                // directly without materializing a second indexed copy in the queue.
                self.charge_message_bytes(bytes.0.len())?;
                snapshot.charge(bytes.0.len())?;
                copy_object.borrow_mut().payload = payload;
            }
            "RegExp" => {
                let regex = payload
                    .as_deref()
                    .unwrap()
                    .downcast_ref::<regex::Regex>()
                    .unwrap();
                copy.put("source", Value::string(regex.as_str()));
                if let Value::String(flags) = value.get("flags") {
                    copy.put("flags", Value::String(flags));
                }
                for key in ["global", "sticky"] {
                    copy.put(key, Value::Bool(value.get(key).truthy()));
                }
                copy.put("lastIndex", Value::Number(0.0));
                copy_object.borrow_mut().payload = payload;
            }
            _ => {}
        }
        Ok(copy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use blitz_dom::DocumentConfig;
    use blitz_html::HtmlDocument;

    struct NoopBridge;
    impl RuntimeBridge for NoopBridge {}

    fn check_message_script(script: &str) {
        let mut document = HtmlDocument::from_html("", DocumentConfig::default()).into();
        let mut guest = Guest::new(NoopBridge);
        guest.evaluate(&mut document, script).unwrap();
        assert!(!guest.drain_jobs(&mut document, 64).unwrap());
        assert!(guest.errors.is_empty(), "{:?}", guest.errors);
        let result = guest.evaluate(&mut document, "passed;").unwrap();
        assert!(matches!(result, Value::Bool(true)), "{result:?}");
    }

    #[test]
    fn snapshots_cyclic_graphs_and_runs_own_getters_at_send_time() {
        check_message_script(
            r#"
            let passed = false;
            let channel = new MessageChannel();
            let reads = 0;
            let shared = Object.create({ inherited: 42 });
            shared.x = 1;
            let array = [0, 1];
            Object.defineProperty(array, "0", {
                enumerable: true,
                get: function () { array[1] = 7; return shared; }
            });
            let data = { a: shared, b: shared, array: array, map: new Map() };
            data.self = data;
            data.map.set(shared, data);
            Object.defineProperty(data, "computed", {
                enumerable: true,
                get: function () { reads++; return shared; }
            });
            Object.defineProperty(data, "hidden", { value: function () {} });
            channel.port2.onmessage = function (e) {
                let copy = e.data;
                passed = e.type === "message" && e.target === channel.port2
                    && copy !== data && copy.self === copy && copy.a === copy.b
                    && copy.a === copy.computed && copy.a !== shared && copy.a.x === 1
                    && copy.a.inherited === undefined && copy.hidden === undefined
                    && Object.getPrototypeOf(copy.a) === Object.prototype
                    && copy.array[0] === copy.a && copy.array[1] === 7
                    && copy.map.get(copy.a) === copy && reads === 1;
            };
            channel.port1.postMessage(data);
            shared.x = 99;
            "#,
        );
    }

    #[test]
    fn copies_or_moves_buffer_backing_and_preserves_view_aliases() {
        check_message_script(
            r#"
            let passed = false;
            let channel = new MessageChannel();
            let buffer = new ArrayBuffer(8);
            let view = new Uint16Array(buffer, 2, 2);
            let alias = view.subarray(1);
            let iterator = alias.values();
            let bytes = new Uint8Array(buffer);
            view.set([4660, 22136]);
            let encoderView = new TextEncoder().encode("abc");
            let sent = { buffer: buffer, view: view, alias: alias, text: encoderView };
            let original = view.buffer === buffer && alias.buffer === buffer
                && bytes[2] === 52 && alias[0] === 22136;
            let unused = new ArrayBuffer(1);
            let copied;
            let senderDetached = false;
            channel.port2.onmessage = function (e) {
                let data = e.data;
                if (copied === undefined) {
                    copied = data;
                    return;
                }
                data.alias[0] = 7;
                passed = original && senderDetached && copied.buffer !== data.buffer
                    && copied.view[0] === 4660 && data.view[0] === 9
                    && data.view[1] === 7 && copied.alias[0] === 22136
                    && data.view.buffer === data.buffer && data.alias.buffer === data.buffer
                    && data.view.byteOffset === 2 && data.alias.byteOffset === 4
                    && data.buffer.byteLength === 8 && data.text[1] === 98
                    && e.ports.length === 0;
            };
            channel.port1.postMessage(sent);
            view[0] = 9;
            channel.port1.postMessage(sent, { transfer: [buffer, encoderView.buffer, unused] });
            let constructorRejected = false;
            try { new Uint8Array(buffer); }
            catch (e) { constructorRejected = e.name === "TypeError"; }
            let iteratorRejected = false;
            try { iterator.next(); }
            catch (e) { iteratorRejected = e.name === "TypeError"; }
            senderDetached = constructorRejected && iteratorRejected
                && buffer.byteLength === 0 && unused.byteLength === 0
                && view.length === 0 && alias.byteLength === 0 && alias.byteOffset === 0
                && bytes[2] === undefined && !(2 in bytes) && Object.keys(bytes).length === 0
                && encoderView.length === 0;
            "#,
        );
    }

    #[test]
    fn moves_pending_messages_and_exposes_transferred_ports_in_order() {
        check_message_script(
            r#"
            let passed = false;
            let carrier = new MessageChannel();
            let channel = new MessageChannel();
            let extra = new MessageChannel();
            let oldCalls = 0;
            let received = "";
            let identities = false;
            channel.port2.onmessage = function () { oldCalls++; };
            channel.port1.onmessage = function (e) {
                passed = identities && oldCalls === 0 && received === "ab" && e.data === "reply";
            };
            channel.port1.postMessage("a");
            carrier.port2.onmessage = function (e) {
                let moved = e.ports[0];
                identities = e.ports.length === 2 && moved === e.data.port
                    && moved !== channel.port2 && e.ports[1] !== extra.port2;
                moved.addEventListener("message", function (msg) {
                    received += msg.data;
                    if (received === "ab") { moved.postMessage("reply"); }
                });
                moved.start();
            };
            carrier.port1.postMessage({ port: channel.port2 }, [channel.port2, extra.port2]);
            channel.port2.close();
            channel.port2.postMessage("stale");
            channel.port1.postMessage("b");
            "#,
        );
    }

    #[test]
    fn closing_receiver_releases_discarded_cycles_and_transferred_inboxes() {
        let mut document = HtmlDocument::from_html("", DocumentConfig::default()).into();
        let mut guest = Guest::new(NoopBridge);
        let carrier = guest
            .evaluate(
                &mut document,
                r#"
                let carrier = new MessageChannel();
                let inner = new MessageChannel();
                let cyclic = {};
                cyclic.self = cyclic;
                inner.port1.postMessage(cyclic);
                carrier.port1.postMessage(cyclic, [inner.port2]);
                carrier;
                "#,
            )
            .unwrap();
        let (outer_copy, inner_copy, transferred) = {
            let receiver = port_state(&carrier.get("port2")).unwrap();
            let receiver = receiver.borrow();
            let outer_message = receiver.inbox.front().unwrap();
            let outer_copy = Rc::downgrade(&outer_message.data.as_object().unwrap());
            let transferred = port_state(&outer_message.ports[0]).unwrap();
            let inner_copy = {
                let state = transferred.borrow();
                let data = &state.inbox.front().unwrap().data;
                Rc::downgrade(&data.as_object().unwrap())
            };
            (outer_copy, inner_copy, Rc::downgrade(&transferred))
        };

        guest.evaluate(&mut document, "carrier.port2.close();").unwrap();

        assert!(outer_copy.upgrade().is_none());
        assert!(inner_copy.upgrade().is_none());
        assert!(transferred.upgrade().is_none());
        assert_eq!(guest.pending_message_bytes.get(), 0);
    }

    #[test]
    fn transferring_the_destination_discards_delivery_after_detaching() {
        check_message_script(
            r#"
            let channel = new MessageChannel();
            let control = new MessageChannel();
            let calls = 0;
            let passed = false;
            let buffer = new ArrayBuffer(1);
            channel.port2.onmessage = function () { calls++; };
            channel.port1.postMessage("pending");
            channel.port1.postMessage(channel.port2, [buffer, channel.port2]);
            control.port2.onmessage = function () {
                passed = buffer.byteLength === 0 && calls === 0;
            };
            control.port1.postMessage(1);
            "#,
        );
    }

    #[test]
    fn rejects_invalid_transfers_without_detaching_other_entries() {
        check_message_script(
            r#"
            let passed = false;
            let carrier = new MessageChannel();
            let channel = new MessageChannel();
            let buffer = new ArrayBuffer(4);
            let nested = new ArrayBuffer(2);
            let checks = 0;
            function expectCloneError(message, list) {
                try { carrier.port1.postMessage(message, list); }
                catch (e) {
                    if (e.name === "DataCloneError" && e.code === 25 && e instanceof DOMException) {
                        checks++;
                    }
                }
            }
            expectCloneError(1, [buffer, buffer]);
            expectCloneError({ nested: [function () {}] }, [buffer, channel.port2]);
            expectCloneError(channel.port2, [buffer]);
            expectCloneError(1, [buffer, new Uint8Array(buffer)]);
            expectCloneError(1, [buffer, carrier.port1]);
            let reentrant = {};
            Object.defineProperty(reentrant, "getter", {
                enumerable: true,
                get: function () {
                    carrier.port1.postMessage("inner", [nested]);
                    return 1;
                }
            });
            expectCloneError(reentrant, [buffer, nested, channel.port2]);
            carrier.port2.onmessage = function (e) {
                if (e.data !== "inner") { checks = -100; }
            };
            channel.port2.onmessage = function (e) {
                passed = e.data === 42 && checks === 6
                    && buffer.byteLength === 4 && nested.byteLength === 0;
            };
            channel.port1.postMessage(42);
            "#,
        );
    }
}

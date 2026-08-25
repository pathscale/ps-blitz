use std::collections::VecDeque;
use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError, TryRecvError};
use std::time::{Duration, Instant};

use anyrender::render_to_buffer;
use anyrender_vello_cpu::VelloCpuImageRenderer;
use base64::Engine;
use blitz_debug_control::{ControlRequest, ControlResponse, DebugServer, ServerConfig};
use blitz_dom::Document;
use blitz_dom::NodeId;
use blitz_paint::paint_scene;
use blitz_traits::events::{
    BlitzImeEvent, BlitzKeyEvent, BlitzPointerEvent, BlitzPointerId, KeyState, MouseEventButton,
    MouseEventButtons, Point, PointerCoords, PointerDetails, UiEvent,
};
use keyboard_types::{Code, Key, Location, Modifiers};
use serde_json::{Value, json};

use crate::ScriptDocument;

const ELEMENT_KEY: &str = "element-6066-11e4-a52e-4f735466cecf";
const MAX_IDLE_TURNS: usize = 1_000;
const ASYNC_SCRIPT_TIMEOUT: Duration = Duration::from_secs(5);
const EVENT_TRACE_CAPACITY: usize = 256;

/// UI-thread half of the debug-control channel.
///
/// The networking thread can only create [`ControlRequest`] values. Call one
/// of the `service_*` methods from the thread that owns `ScriptDocument`.
pub struct DebugController {
    server: DebugServer,
    requests: Receiver<ControlRequest>,
    document_generation: u64,
    document_revision: u64,
    style_revision: u64,
    layout_revision: u64,
    paint_revision: u64,
    screenshot_size: Option<(u32, u32)>,
    latest_screenshot: Option<String>,
    next_async_id: u64,
    next_event_sequence: u64,
    event_traces: VecDeque<Value>,
    action_pointer: (f32, f32),
    action_buttons: MouseEventButtons,
    exit_requested: bool,
}

impl DebugController {
    pub fn start(config: ServerConfig) -> io::Result<Self> {
        let (server, requests) = DebugServer::start(config)?;
        Ok(Self {
            server,
            requests,
            document_generation: 1,
            document_revision: 1,
            style_revision: 0,
            layout_revision: 0,
            paint_revision: 0,
            screenshot_size: None,
            latest_screenshot: None,
            next_async_id: 1,
            next_event_sequence: 1,
            event_traces: VecDeque::new(),
            action_pointer: (0.0, 0.0),
            action_buttons: MouseEventButtons::default(),
            exit_requested: false,
        })
    }

    /// Tell the server how to wake the thread that calls `service_pending`.
    ///
    /// An embedder with an event loop should install this, then service on
    /// wake. Without it the server has no way to say a request has arrived and
    /// the embedder is back to polling. Harnesses that call `service_one` block
    /// on the channel and need nothing here.
    pub fn set_waker(&self, wake: impl Fn() + Send + Sync + 'static) {
        self.server.waker().set(wake);
    }

    /// Enable CPU screenshots and frame commits at the supplied viewport size.
    pub fn with_cpu_screenshot(mut self, width: u32, height: u32) -> Self {
        self.screenshot_size = Some((width, height));
        self
    }

    /// Start only when both debug-control environment settings are present.
    pub fn start_from_env(renderer_revision: impl Into<String>) -> io::Result<Option<Self>> {
        let Some(address) = std::env::var_os("TAURI_BLITZ_DRIVER") else {
            return Ok(None);
        };
        let Some(descriptor) = std::env::var_os("TAURI_BLITZ_DRIVER_DESCRIPTOR") else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "TAURI_BLITZ_DRIVER_DESCRIPTOR is required",
            ));
        };
        let bind_address: SocketAddr = address.to_string_lossy().parse().map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid TAURI_BLITZ_DRIVER address: {error}"),
            )
        })?;
        Self::start(ServerConfig {
            bind_address,
            descriptor_path: PathBuf::from(descriptor),
            renderer_revision: renderer_revision.into(),
        })
        .map(Some)
    }

    /// Service every request currently queued without blocking.
    pub fn service_pending(&mut self, document: &mut ScriptDocument) -> usize {
        let mut count = 0;
        loop {
            match self.requests.try_recv() {
                Ok(request) => {
                    self.handle(document, request);
                    count += 1;
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => return count,
            }
        }
    }

    /// Wait for and service one request. Useful for headless harnesses.
    pub fn service_one(
        &mut self,
        document: &mut ScriptDocument,
        timeout: Duration,
    ) -> Result<bool, RecvTimeoutError> {
        match self.requests.recv_timeout(timeout) {
            Ok(request) => {
                self.handle(document, request);
                Ok(true)
            }
            Err(RecvTimeoutError::Disconnected) => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// Whether the authenticated debug client requested a clean harness exit.
    pub fn exit_requested(&self) -> bool {
        self.exit_requested
    }

    fn handle(&mut self, document: &mut ScriptDocument, request: ControlRequest) {
        let response = self.route(document, &request);
        let _ = request.respond(response);
    }

    fn route(
        &mut self,
        document: &mut ScriptDocument,
        request: &ControlRequest,
    ) -> ControlResponse {
        match (request.method.as_str(), request.path.as_str()) {
            ("GET", "url") => success(json!(
                document
                    .current_url()
                    .map(ToString::to_string)
                    .unwrap_or_default()
            )),
            ("GET", "source") => success(json!(document.page_source())),
            ("GET", "screenshot") => self.screenshot(document),
            ("GET", "window") => success(json!("blitz-main")),
            ("GET", "window/handles") => success(json!(["blitz-main"])),
            ("POST", "window") => match request.body.get("handle").and_then(Value::as_str) {
                Some("blitz-main") => success(Value::Null),
                _ => error("no such window", "only the blitz-main window exists"),
            },
            ("POST", "execute/sync") => self.execute_sync(document, &request.body),
            ("POST", "execute/async") => self.execute_async(document, &request.body),
            ("POST", "actions") => self.perform_actions(document, &request.body),
            ("DELETE", "actions") => success(Value::Null),
            ("POST", "element") => self.find(document, &request.body, false),
            ("POST", "elements") => self.find(document, &request.body, true),
            ("POST", "blitz/waitForIdle") => self.wait_for_idle(document),
            ("GET", "blitz/getDomSnapshot") => self.dom_snapshot(document),
            ("GET" | "POST", "blitz/getConsoleEntries") => {
                self.console_entries(document, &request.body)
            }
            ("GET" | "POST", "blitz/getRuntimeErrors") => {
                self.runtime_errors(document, &request.body)
            }
            ("GET" | "POST", "blitz/traceEvent") => self.event_trace(&request.body),
            ("GET" | "POST", "blitz/getComputedStyle") => {
                self.computed_style(document, &request.body)
            }
            ("GET", "blitz/getLayoutTree") => self.layout_tree(document),
            ("GET", "blitz/getSvgDiagnostics") => self.svg_diagnostics(document),
            ("GET", "blitz/getRendererMetrics") => self.renderer_metrics(),
            ("POST", "blitz/shutdown") => {
                self.exit_requested = true;
                success(Value::Null)
            }
            _ => self.element_command(document, request),
        }
    }

    fn computed_style(&mut self, document: &mut ScriptDocument, body: &Value) -> ControlResponse {
        let Some(reference) = body.get("element").and_then(Value::as_str) else {
            return invalid("blitz/getComputedStyle requires an element reference");
        };
        let Some(node_id) = self.resolve_element(document, reference) else {
            return error("stale element reference", "element is no longer attached");
        };
        document.inner_mut().resolve(0.0);
        self.style_revision += 1;
        let inner = document.inner();
        let Some(properties) = inner
            .get_node(node_id)
            .and_then(|node| node.diagnostic_computed_style())
        else {
            return error("unknown error", "computed style is unavailable");
        };
        success(json!({
            "nodeId": node_id,
            "styleRevision": self.style_revision,
            "properties": properties.into_iter()
                .map(|(name, value)| (name.to_string(), Value::String(value)))
                .collect::<serde_json::Map<String, Value>>(),
        }))
    }

    fn layout_tree(&mut self, document: &mut ScriptDocument) -> ControlResponse {
        document.inner_mut().resolve(0.0);
        self.layout_revision += 1;
        let inner = document.inner();
        let nodes = inner
            .tree()
            .iter()
            .filter_map(|(node_id, _)| {
                inner.get_client_bounding_rect(node_id).map(|rect| {
                    json!({
                        "nodeId": node_id,
                        "x": rect.x,
                        "y": rect.y,
                        "width": rect.width,
                        "height": rect.height,
                    })
                })
            })
            .collect::<Vec<_>>();
        success(json!({"layoutRevision": self.layout_revision, "nodes": nodes}))
    }

    fn renderer_metrics(&self) -> ControlResponse {
        success(json!({
            "documentRevision": self.document_revision,
            "styleRevision": self.style_revision,
            "layoutRevision": self.layout_revision,
            "paintRevision": self.paint_revision,
            "eventTraceEntries": self.event_traces.len(),
        }))
    }

    fn svg_diagnostics(&mut self, document: &mut ScriptDocument) -> ControlResponse {
        document.inner_mut().resolve(0.0);
        let inner = document.inner();
        let entries = inner
            .tree()
            .iter()
            .filter_map(|(node_id, node)| {
                let element = node.element_data()?;
                if element.name.local != blitz_dom::local_name!("svg")
                    || !node.flags.is_in_document()
                {
                    return None;
                }
                let source = inner.debug_inline_svg_source(node_id)?;
                let rect = inner.get_client_bounding_rect(node_id);
                let image = match element.image_data() {
                    Some(blitz_dom::node::ImageData::Svg(svg)) => json!({
                        "kind": "svg",
                        "width": svg.tree.size().width(),
                        "height": svg.tree.size().height(),
                        "intrinsicWidth": svg.intrinsic_width(),
                        "intrinsicHeight": svg.intrinsic_height(),
                        "rootChildren": svg.tree.root().children().len(),
                        "contentBounds": {
                            "x": svg.tree.root().layer_bounding_box().x(),
                            "y": svg.tree.root().layer_bounding_box().y(),
                            "width": svg.tree.root().layer_bounding_box().width(),
                            "height": svg.tree.root().layer_bounding_box().height(),
                        },
                    }),
                    Some(blitz_dom::node::ImageData::Raster(raster)) => json!({
                        "kind": "raster",
                        "width": raster.width,
                        "height": raster.height,
                    }),
                    Some(blitz_dom::node::ImageData::None) => json!({"kind": "none"}),
                    None => Value::Null,
                };
                Some(json!({
                    "nodeId": node_id,
                    "source": source,
                    "layout": rect.map(|rect| json!({
                        "x": rect.x,
                        "y": rect.y,
                        "width": rect.width,
                        "height": rect.height,
                    })),
                    "image": image,
                }))
            })
            .collect::<Vec<_>>();
        success(json!({"entries": entries}))
    }

    fn perform_actions(&mut self, document: &mut ScriptDocument, body: &Value) -> ControlResponse {
        let Some(sources) = body.get("actions").and_then(Value::as_array) else {
            return invalid("actions must be an array");
        };
        for source in sources {
            let source_type = source.get("type").and_then(Value::as_str).unwrap_or("");
            let Some(actions) = source.get("actions").and_then(Value::as_array) else {
                return invalid("each input source requires an actions array");
            };
            for action in actions {
                let action_type = action.get("type").and_then(Value::as_str).unwrap_or("");
                match (source_type, action_type) {
                    (_, "pause") => {}
                    ("pointer", "pointerMove") => {
                        let Some(x) = action.get("x").and_then(Value::as_f64) else {
                            return invalid("pointerMove requires x");
                        };
                        let Some(y) = action.get("y").and_then(Value::as_f64) else {
                            return invalid("pointerMove requires y");
                        };
                        if action
                            .get("origin")
                            .and_then(Value::as_str)
                            .is_some_and(|origin| origin != "viewport")
                        {
                            return ControlResponse::unsupported(
                                "only viewport-origin pointer actions are supported",
                            );
                        }
                        self.action_pointer = (x as f32, y as f32);
                        document.handle_ui_event(UiEvent::PointerMove(action_pointer_event(
                            self.action_pointer,
                            self.action_buttons,
                        )));
                    }
                    ("pointer", "pointerDown") => {
                        if action.get("button").and_then(Value::as_u64).unwrap_or(0) != 0 {
                            return ControlResponse::unsupported(
                                "only the primary pointer button is supported",
                            );
                        }
                        self.action_buttons = MouseEventButtons::Primary;
                        document.handle_ui_event(UiEvent::PointerDown(action_pointer_event(
                            self.action_pointer,
                            self.action_buttons,
                        )));
                    }
                    ("pointer", "pointerUp") => {
                        let event = action_pointer_event(self.action_pointer, self.action_buttons);
                        document.handle_ui_event(UiEvent::PointerUp(event));
                        self.action_buttons = MouseEventButtons::default();
                    }
                    ("key", "keyDown") => {
                        let Some(value) = action.get("value").and_then(Value::as_str) else {
                            return invalid("keyDown requires value");
                        };
                        document.handle_ui_event(UiEvent::KeyDown(key_event(value, true)));
                    }
                    ("key", "keyUp") => {
                        let Some(value) = action.get("value").and_then(Value::as_str) else {
                            return invalid("keyUp requires value");
                        };
                        document.handle_ui_event(UiEvent::KeyUp(key_event(value, false)));
                    }
                    _ => return ControlResponse::unsupported("input action is not implemented"),
                }
            }
        }
        self.document_revision += 1;
        success(Value::Null)
    }

    fn execute_sync(&mut self, document: &mut ScriptDocument, body: &Value) -> ControlResponse {
        let Some(script) = body.get("script").and_then(Value::as_str) else {
            return invalid("missing script");
        };
        let args = body.get("args").cloned().unwrap_or_else(|| json!([]));
        if !args.is_array() {
            return invalid("args must be an array");
        }
        let source = format!("(function() {{\n{script}\n}}).apply(null, {args})");
        match document.eval_json(&source) {
            Ok(value) => {
                self.document_revision += 1;
                success(value)
            }
            Err(message) => error("javascript error", message),
        }
    }

    fn execute_async(&mut self, document: &mut ScriptDocument, body: &Value) -> ControlResponse {
        let Some(script) = body.get("script").and_then(Value::as_str) else {
            return invalid("missing script");
        };
        let args = body.get("args").cloned().unwrap_or_else(|| json!([]));
        let Some(args) = args.as_array() else {
            return invalid("args must be an array");
        };
        let result_name = format!("__blitzAsyncResult{}", self.next_async_id);
        self.next_async_id += 1;
        let args_source = serde_json::to_string(args).unwrap();
        let source = format!(
            "globalThis.{result_name} = {{ done: false }};\n\
             (function() {{\n{script}\n}}).apply(null, [...{args_source}, value => {{\
               globalThis.{result_name} = {{ done: true, value }};\
             }}]);\nnull"
        );
        if let Err(message) = document.eval_json(&source) {
            return error("javascript error", message);
        }

        let deadline = Instant::now() + ASYNC_SCRIPT_TIMEOUT;
        loop {
            document.poll(None);
            match document.eval_json(&format!("globalThis.{result_name}")) {
                Ok(state) if state.get("done").and_then(Value::as_bool) == Some(true) => {
                    let value = state.get("value").cloned().unwrap_or(Value::Null);
                    let _ = document.eval_json(&format!("delete globalThis.{result_name}"));
                    self.document_revision += 1;
                    return success(value);
                }
                Ok(_) => {}
                Err(message) => return error("javascript error", message),
            }
            if Instant::now() >= deadline {
                let _ = document.eval_json(&format!("delete globalThis.{result_name}"));
                return error("script timeout", "asynchronous script callback did not run");
            }
            std::thread::yield_now();
        }
    }

    fn console_entries(&self, document: &ScriptDocument, body: &Value) -> ControlResponse {
        let after = body.get("after").and_then(Value::as_u64).unwrap_or(0);
        let entries = document.console_entries_after(after);
        let overflowed = entries
            .first()
            .is_some_and(|entry| entry.sequence > after.saturating_add(1));
        success(json!({
            "after": after,
            "overflowed": overflowed,
            "entries": entries.into_iter().map(|entry| json!({
                "sequence": entry.sequence,
                "level": entry.level,
                "message": entry.message,
            })).collect::<Vec<_>>(),
        }))
    }

    fn runtime_errors(&self, document: &ScriptDocument, body: &Value) -> ControlResponse {
        let after = body.get("after").and_then(Value::as_u64).unwrap_or(0);
        let entries = document.runtime_errors_after(after);
        let overflowed = entries
            .first()
            .is_some_and(|entry| entry.sequence > after.saturating_add(1));
        success(json!({
            "after": after,
            "overflowed": overflowed,
            "entries": entries.into_iter().map(|entry| json!({
                "sequence": entry.sequence,
                "message": entry.message,
                "stack": entry.stack,
            })).collect::<Vec<_>>(),
        }))
    }

    fn event_trace(&self, body: &Value) -> ControlResponse {
        let after = body.get("after").and_then(Value::as_u64).unwrap_or(0);
        let entries = self
            .event_traces
            .iter()
            .filter(|entry| {
                entry["sequence"]
                    .as_u64()
                    .is_some_and(|value| value > after)
            })
            .cloned()
            .collect::<Vec<_>>();
        success(json!({"after": after, "entries": entries}))
    }

    fn find(&self, document: &ScriptDocument, body: &Value, many: bool) -> ControlResponse {
        if body.get("using").and_then(Value::as_str) != Some("css selector") {
            return invalid("only the css selector locator strategy is supported");
        }
        let Some(selector) = body.get("value").and_then(Value::as_str) else {
            return invalid("missing selector value");
        };
        let inner = document.inner();
        if many {
            match inner.query_selector_all(selector) {
                Ok(ids) => success(Value::Array(
                    ids.iter().map(|id| self.element_reference(*id)).collect(),
                )),
                Err(error) => invalid(format!("invalid CSS selector: {error:?}")),
            }
        } else {
            match inner.query_selector(selector) {
                Ok(Some(id)) => success(self.element_reference(id)),
                Ok(None) => error(
                    "no such element",
                    format!("no element matches {selector:?}"),
                ),
                Err(error) => invalid(format!("invalid CSS selector: {error:?}")),
            }
        }
    }

    /// A reference a client can hold across requests.
    ///
    /// This used to carry a separate `instance_id` because a node id was a bare
    /// slot index that a later node could reuse. `NodeId` is versioned now and
    /// carries that itself, so the raw `u64` is the whole reference.
    fn element_reference(&self, node_id: NodeId) -> Value {
        json!({ELEMENT_KEY: format!("{}:{}", self.document_generation, node_id.as_u64())})
    }

    fn element_command(
        &mut self,
        document: &mut ScriptDocument,
        request: &ControlRequest,
    ) -> ControlResponse {
        let Some(rest) = request.path.strip_prefix("element/") else {
            return ControlResponse::unsupported("command is not implemented");
        };
        let Some((reference, command)) = rest.split_once('/') else {
            return invalid("invalid element command path");
        };
        let Some(node_id) = self.resolve_element(document, reference) else {
            return error("stale element reference", "element is no longer attached");
        };
        if request.method == "GET" {
            if let Some(name) = command.strip_prefix("attribute/") {
                return self.element_attribute(document, node_id, name);
            }
            if let Some(name) = command.strip_prefix("property/") {
                return self.element_property(document, node_id, name);
            }
        }
        match (request.method.as_str(), command) {
            ("GET", "text") => {
                let inner = document.inner();
                success(json!(inner.get_node(node_id).unwrap().text_content()))
            }
            ("GET", "rect") => {
                let inner = document.inner();
                let Some(rect) = inner.get_client_bounding_rect(node_id) else {
                    return error("stale element reference", "element has no layout box");
                };
                success(json!({
                    "x": rect.x,
                    "y": rect.y,
                    "width": rect.width,
                    "height": rect.height,
                }))
            }
            ("POST", "click") => self.click(document, node_id),
            ("POST", "value") => self.send_keys(document, node_id, &request.body),
            ("POST", "focus") => {
                document.inner_mut().set_focus_to(node_id);
                success(Value::Null)
            }
            ("GET", "displayed") => {
                document.inner_mut().resolve(0.0);
                let inner = document.inner();
                let displayed = inner.get_node(node_id).is_some_and(|node| {
                    !node.is_display_none()
                        && inner
                            .get_client_bounding_rect(node_id)
                            .is_some_and(|rect| rect.width > 0.0 && rect.height > 0.0)
                });
                success(json!(displayed))
            }
            ("GET", "enabled") => {
                let inner = document.inner();
                let enabled = inner.get_node(node_id).is_some_and(|node| {
                    node.attrs().is_none_or(|attributes| {
                        !attributes
                            .iter()
                            .any(|attribute| &*attribute.name.local == "disabled")
                    })
                });
                success(json!(enabled))
            }
            _ => ControlResponse::unsupported("element command is not implemented"),
        }
    }

    fn element_attribute(
        &self,
        document: &ScriptDocument,
        node_id: NodeId,
        name: &str,
    ) -> ControlResponse {
        let inner = document.inner();
        let value = inner
            .get_node(node_id)
            .and_then(|node| node.attrs())
            .and_then(|attributes| {
                attributes
                    .iter()
                    .find(|attribute| &*attribute.name.local == name)
            })
            .map(|attribute| attribute.value.to_string());
        success(value.map(Value::String).unwrap_or(Value::Null))
    }

    fn element_property(
        &self,
        document: &ScriptDocument,
        node_id: NodeId,
        name: &str,
    ) -> ControlResponse {
        let inner = document.inner();
        let Some(node) = inner.get_node(node_id) else {
            return error("stale element reference", "element is no longer attached");
        };
        let value = match name {
            "textContent" => Value::String(node.text_content()),
            "className" => node
                .attr(blitz_dom::local_name!("class"))
                .map(|value| Value::String(value.to_string()))
                .unwrap_or(Value::Null),
            "value" => node
                .element_data()
                .and_then(|element| element.text_input_data())
                .map(|input| Value::String(input.editor.text().to_string()))
                .unwrap_or(Value::Null),
            "checked" => node
                .element_data()
                .and_then(|element| element.checkbox_input_checked())
                .map(Value::Bool)
                .unwrap_or(Value::Bool(false)),
            _ => {
                drop(inner);
                return self.element_attribute(document, node_id, name);
            }
        };
        success(value)
    }

    fn resolve_element(&self, document: &ScriptDocument, reference: &str) -> Option<NodeId> {
        let mut parts = reference.split(':');
        let generation = parts.next()?;
        let node_id = parts.next()?;
        if parts.next().is_some() {
            return None;
        }
        if generation.parse::<u64>().ok()? != self.document_generation {
            return None;
        }
        // A stale id fails here rather than resolving: the version bits no
        // longer match whatever node now holds the slot.
        let node_id = NodeId::from_u64(node_id.parse::<u64>().ok()?);
        document
            .inner()
            .get_node(node_id)
            .filter(|node| node.flags.is_in_document())
            .map(|_| node_id)
    }

    fn click(&mut self, document: &mut ScriptDocument, node_id: NodeId) -> ControlResponse {
        document.inner_mut().resolve(0.0);
        let Some(rect) = document.inner().get_client_bounding_rect(node_id) else {
            return error("element not interactable", "element has no layout box");
        };
        if rect.width <= 0.0 || rect.height <= 0.0 {
            return error(
                "element not interactable",
                "element has an empty layout box",
            );
        }
        let x = (rect.x + rect.width / 2.0) as f32;
        let y = (rect.y + rect.height / 2.0) as f32;
        let (hit_node_id, path, document_node_id) = {
            let inner = document.inner();
            let hit_node_id = inner.hit(x, y).map(|hit| hit.node_id).unwrap_or(node_id);
            (
                hit_node_id,
                inner.node_chain(hit_node_id),
                inner.root_node().id,
            )
        };
        let pointer = pointer_event(x, y);
        document.handle_ui_event(UiEvent::PointerMove(pointer.clone()));
        document.handle_ui_event(UiEvent::PointerDown(pointer.clone()));
        document.handle_ui_event(UiEvent::PointerUp(pointer));
        self.push_event_trace(json!({
            "sequence": self.next_event_sequence,
            "event": "click",
            "requestedNodeId": node_id,
            "targetNodeId": hit_node_id,
            "path": path,
            "includedDocument": path.contains(&document_node_id),
            "inputPath": "pointer-hit-test",
        }));
        self.document_revision += 1;
        success(Value::Null)
    }

    fn send_keys(
        &mut self,
        document: &mut ScriptDocument,
        node_id: NodeId,
        body: &Value,
    ) -> ControlResponse {
        let text = body
            .get("text")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| {
                body.get("value")
                    .and_then(Value::as_array)
                    .map(|parts| parts.iter().filter_map(Value::as_str).collect::<String>())
            });
        let Some(text) = text else {
            return invalid("send keys requires text or a string value array");
        };
        document.inner_mut().resolve(0.0);
        document.inner_mut().set_focus_to(node_id);
        document.handle_ui_event(UiEvent::Ime(BlitzImeEvent::Commit(text)));
        self.document_revision += 1;
        success(Value::Null)
    }

    fn push_event_trace(&mut self, entry: Value) {
        if self.event_traces.len() == EVENT_TRACE_CAPACITY {
            self.event_traces.pop_front();
        }
        self.event_traces.push_back(entry);
        self.next_event_sequence += 1;
    }

    fn wait_for_idle(&mut self, document: &mut ScriptDocument) -> ControlResponse {
        let mut turns = 0;
        while document.poll(None) {
            turns += 1;
            if turns >= MAX_IDLE_TURNS {
                return error(
                    "timeout",
                    "document did not become idle within the work budget",
                );
            }
        }
        document.inner_mut().resolve(0.0);
        self.style_revision += 1;
        self.layout_revision += 1;
        if let Err(response) = self.commit_cpu_frame(document) {
            return response;
        }
        success(json!({
            "documentRevision": self.document_revision,
            "styleRevision": self.style_revision,
            "layoutRevision": self.layout_revision,
            "paintRevision": self.paint_revision,
            "workTurns": turns,
        }))
    }

    fn screenshot(&mut self, document: &mut ScriptDocument) -> ControlResponse {
        if let ControlResponse::Error {
            error,
            message,
            stacktrace,
        } = self.wait_for_idle(document)
        {
            return ControlResponse::Error {
                error,
                message,
                stacktrace,
            };
        }
        match self.latest_screenshot.clone() {
            Some(encoded) => success(Value::String(encoded)),
            None => ControlResponse::unsupported("CPU screenshots are not configured"),
        }
    }

    fn commit_cpu_frame(&mut self, document: &mut ScriptDocument) -> Result<(), ControlResponse> {
        let Some((width, height)) = self.screenshot_size else {
            return Ok(());
        };
        let mut inner = document.inner_mut();
        // The document's own scale, not a hardcoded 1.0.
        //
        // A HiDPI window is laid out at 2.0 and painted by the renderer at 2.0;
        // a screenshot taken at 1.0 was a CSS-pixel image of it, at half the
        // resolution the window actually shows. Legible, and not the picture
        // anyone means when they ask for a screenshot of that window.
        //
        // The buffer is sized to match, so the result is a device-pixel image
        // of the requested CSS viewport, the way a browser screenshot is.
        //
        let previous = inner.viewport().clone();
        let scale = previous.scale_f64();
        let (device_width, device_height) = (
            ((f64::from(width) * scale).round() as u32).max(1),
            ((f64::from(height) * scale).round() as u32).max(1),
        );

        // Lay out for the size being painted, which is the fix for text
        // overflowing its boxes in a screenshot.
        //
        // This used to paint into a `width * height` buffer while leaving the
        // viewport at whatever the window was, so a screenshot requested at a
        // size the window does not have laid the document out for one geometry
        // and painted it into another. Text positioned for a wider viewport
        // then ran past the edges of boxes drawn for a narrower one, which
        // looks like a font or a scale fault and is neither.
        //
        // `tests/screenshot_scale.rs` had already ruled out glyph-versus-paint
        // scale, correctly: a document laid out at 2.0 and painted at 1.0 keeps
        // its text inside its boxes. What it did not vary was the viewport's
        // *dimensions*, which is where the mismatch actually was.
        //
        // Restored below, because a screenshot must not reflow the live window.
        // The relayout costs one `resolve` at each size; a screenshot is an
        // explicit debugging request, not a per-frame cost.
        inner.set_viewport(blitz_traits::shell::Viewport::new(
            width,
            height,
            scale as f32,
            previous.color_scheme,
        ));
        inner.resolve(0.0);

        let buffer = render_to_buffer::<VelloCpuImageRenderer, _>(
            |scene| paint_scene(scene, &mut inner, scale, device_width, device_height, 0, 0),
            device_width,
            device_height,
        );

        inner.set_viewport(previous);
        inner.resolve(0.0);
        drop(inner);

        let mut png = Vec::new();
        let encoder = image::codecs::png::PngEncoder::new(&mut png);
        image::ImageEncoder::write_image(
            encoder,
            &buffer,
            device_width,
            device_height,
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|encode_error| {
            error(
                "unknown error",
                format!("PNG encoding failed: {encode_error}"),
            )
        })?;
        self.latest_screenshot = Some(base64::engine::general_purpose::STANDARD.encode(png));
        self.paint_revision += 1;
        Ok(())
    }

    fn dom_snapshot(&self, document: &ScriptDocument) -> ControlResponse {
        let inner = document.inner();
        let nodes = inner
            .tree()
            .iter()
            .map(|(id, node)| {
                let element = node.element_data();
                let attributes = element
                    .map(|element| {
                        element
                            .attrs
                            .iter()
                            .map(|attribute| {
                                (
                                    attribute.name.local.to_string(),
                                    Value::String(attribute.value.to_string()),
                                )
                            })
                            .collect::<serde_json::Map<String, Value>>()
                    })
                    .unwrap_or_default();
                json!({
                    "nodeId": id,
                    "parentId": node.parent,
                    "children": node.children.iter().copied().collect::<Vec<_>>(),
                    "tagName": element.map(|value| value.name.local.to_string()),
                    "attributes": attributes,
                    "text": node.text_content(),
                })
            })
            .collect::<Vec<_>>();
        success(json!({
            "documentGeneration": self.document_generation,
            "documentRevision": self.document_revision,
            "styleRevision": self.style_revision,
            "layoutRevision": self.layout_revision,
            "paintRevision": self.paint_revision,
            "nodes": nodes,
        }))
    }
}

fn pointer_event(x: f32, y: f32) -> BlitzPointerEvent {
    action_pointer_event((x, y), MouseEventButtons::from(MouseEventButton::Main))
}

fn action_pointer_event((x, y): (f32, f32), buttons: MouseEventButtons) -> BlitzPointerEvent {
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

fn key_event(value: &str, pressed: bool) -> BlitzKeyEvent {
    let (key, code, text) = match value {
        "\u{e003}" => (Key::Backspace, Code::Backspace, None),
        "\u{e007}" | "\n" | "\r" => (Key::Enter, Code::Enter, None),
        value => (
            Key::Character(value.into()),
            Code::Unidentified,
            Some(value.into()),
        ),
    };
    BlitzKeyEvent {
        key,
        code,
        modifiers: Modifiers::empty(),
        location: Location::Standard,
        is_auto_repeating: false,
        is_composing: false,
        state: if pressed {
            KeyState::Pressed
        } else {
            KeyState::Released
        },
        text,
    }
}

fn success(value: Value) -> ControlResponse {
    ControlResponse::Success(value)
}

fn invalid(message: impl Into<String>) -> ControlResponse {
    error("invalid argument", message)
}

fn error(error: impl Into<String>, message: impl Into<String>) -> ControlResponse {
    ControlResponse::Error {
        error: error.into(),
        message: message.into(),
        stacktrace: String::new(),
    }
}

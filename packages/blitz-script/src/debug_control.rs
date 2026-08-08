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
use blitz_paint::paint_scene;
use blitz_traits::events::{
    BlitzPointerEvent, BlitzPointerId, MouseEventButton, MouseEventButtons, Point, PointerCoords,
    PointerDetails, UiEvent,
};
use serde_json::{Value, json};

use crate::ScriptDocument;

const ELEMENT_KEY: &str = "element-6066-11e4-a52e-4f735466cecf";
const MAX_IDLE_TURNS: usize = 1_000;
const ASYNC_SCRIPT_TIMEOUT: Duration = Duration::from_secs(5);

/// UI-thread half of the debug-control channel.
///
/// The networking thread can only create [`ControlRequest`] values. Call one
/// of the `service_*` methods from the thread that owns `ScriptDocument`.
pub struct DebugController {
    _server: DebugServer,
    requests: Receiver<ControlRequest>,
    document_generation: u64,
    document_revision: u64,
    style_revision: u64,
    layout_revision: u64,
    paint_revision: u64,
    screenshot_size: Option<(u32, u32)>,
    latest_screenshot: Option<String>,
    next_async_id: u64,
    exit_requested: bool,
}

impl DebugController {
    pub fn start(config: ServerConfig) -> io::Result<Self> {
        let (server, requests) = DebugServer::start(config)?;
        Ok(Self {
            _server: server,
            requests,
            document_generation: 1,
            document_revision: 1,
            style_revision: 0,
            layout_revision: 0,
            paint_revision: 0,
            screenshot_size: None,
            latest_screenshot: None,
            next_async_id: 1,
            exit_requested: false,
        })
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
            ("POST", "execute/sync") => self.execute_sync(document, &request.body),
            ("POST", "execute/async") => self.execute_async(document, &request.body),
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
            ("POST", "blitz/shutdown") => {
                self.exit_requested = true;
                success(Value::Null)
            }
            _ => self.element_command(document, request),
        }
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
                    ids.iter()
                        .map(|id| {
                            self.element_reference(*id, inner.get_node(*id).unwrap().instance_id)
                        })
                        .collect(),
                )),
                Err(error) => invalid(format!("invalid CSS selector: {error:?}")),
            }
        } else {
            match inner.query_selector(selector) {
                Ok(Some(id)) => {
                    success(self.element_reference(id, inner.get_node(id).unwrap().instance_id))
                }
                Ok(None) => error(
                    "no such element",
                    format!("no element matches {selector:?}"),
                ),
                Err(error) => invalid(format!("invalid CSS selector: {error:?}")),
            }
        }
    }

    fn element_reference(&self, node_id: usize, instance_id: u64) -> Value {
        json!({ELEMENT_KEY: format!("{}:{node_id}:{instance_id}", self.document_generation)})
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
            _ => ControlResponse::unsupported("element command is not implemented"),
        }
    }

    fn resolve_element(&self, document: &ScriptDocument, reference: &str) -> Option<usize> {
        let mut parts = reference.split(':');
        let generation = parts.next()?;
        let node_id = parts.next()?;
        let instance_id = parts.next()?;
        if parts.next().is_some() {
            return None;
        }
        if generation.parse::<u64>().ok()? != self.document_generation {
            return None;
        }
        let node_id = node_id.parse::<usize>().ok()?;
        let instance_id = instance_id.parse::<u64>().ok()?;
        document
            .inner()
            .get_node(node_id)
            .filter(|node| node.instance_id == instance_id)
            .map(|_| node_id)
    }

    fn click(&mut self, document: &mut ScriptDocument, node_id: usize) -> ControlResponse {
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
        let pointer = pointer_event(x, y);
        document.handle_ui_event(UiEvent::PointerMove(pointer.clone()));
        document.handle_ui_event(UiEvent::PointerDown(pointer.clone()));
        document.handle_ui_event(UiEvent::PointerUp(pointer));
        self.document_revision += 1;
        success(Value::Null)
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
        let buffer = render_to_buffer::<VelloCpuImageRenderer, _>(
            |scene| paint_scene(scene, &mut inner, 1.0, width, height, 0, 0),
            width,
            height,
        );
        drop(inner);

        let mut png = Vec::new();
        let encoder = image::codecs::png::PngEncoder::new(&mut png);
        image::ImageEncoder::write_image(
            encoder,
            &buffer,
            width,
            height,
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
                                    Value::String(attribute.value.clone()),
                                )
                            })
                            .collect::<serde_json::Map<String, Value>>()
                    })
                    .unwrap_or_default();
                json!({
                    "nodeId": id,
                    "parentId": node.parent,
                    "children": node.children,
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
        buttons: MouseEventButtons::from(MouseEventButton::Main),
        mods: Default::default(),
        details: PointerDetails::default(),
        element: Point::default(),
        active_pointers: Default::default(),
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

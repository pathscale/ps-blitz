use super::*;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::Instant;

const MAX_BLOB_BYTES: usize = 16 * 1024 * 1024;
const MAX_WEB_ITEMS: usize = 100_000;

#[derive(Clone)]
struct BlobBytes(Vec<u8>);

#[derive(Clone)]
struct PortState {
    inbox: Rc<RefCell<VecDeque<Value>>>,
    peer_queue: Rc<RefCell<VecDeque<Value>>>,
    peer: Option<Value>,
    started: bool,
    closed: bool,
}

#[derive(Clone)]
struct UrlParams {
    pairs: Vec<(String, String)>,
    owner: Option<Value>,
}

fn web_fn(name: &str) -> Value {
    Value::native(format!("Web.{name}"))
}

fn web_object(kind: &str) -> Value {
    Value::object(kind)
}

fn parse_pair_id(name: &str, prefix: &str) -> Result<(u64, usize)> {
    let suffix = name
        .strip_prefix(prefix)
        .ok_or_else(|| Error::type_error("invalid promise reaction"))?;
    let (id, index) = suffix
        .split_once('.')
        .ok_or_else(|| Error::type_error("invalid promise reaction"))?;
    Ok((
        id.parse()
            .map_err(|_| Error::type_error("invalid promise reaction"))?,
        index
            .parse()
            .map_err(|_| Error::type_error("invalid promise reaction"))?,
    ))
}

fn parse_url_params(input: &str) -> Vec<(String, String)> {
    input
        .trim_start_matches('?')
        .split('&')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let (key, value) = part.split_once('=').unwrap_or((part, ""));
            let decode = |value: &str| -> String {
                let replaced = value.replace('+', " ");
                decode_uri_component(&replaced, true).unwrap_or(replaced)
            };
            (decode(key), decode(value))
        })
        .collect()
}

fn url_param_string(params: &[(String, String)]) -> String {
    params
        .iter()
        .map(|(key, value)| {
            let form = |text: &str| encode_uri_component(text, true).replace("%20", "+");
            format!("{}={}", form(key), form(value))
        })
        .collect::<Vec<_>>()
        .join("&")
}

fn url_params(value: &Value) -> Vec<(String, String)> {
    value
        .as_object()
        .and_then(|object| {
            object
                .borrow()
                .payload
                .as_ref()
                .and_then(|payload| payload.downcast_ref::<UrlParams>().cloned())
        })
        .map_or_else(Vec::new, |params| params.pairs)
}

fn set_url_params(value: &Value, params: Vec<(String, String)>, owner: Option<Value>) {
    if let Some(object) = value.as_object() {
        object.borrow_mut().payload = Some(Rc::new(UrlParams {
            pairs: params.clone(),
            owner: owner.clone(),
        }));
    }
    if let Some(owner) = owner {
        if let Ok(mut url) = url::Url::parse(&owner.get("href").text()) {
            url.query_pairs_mut().clear().extend_pairs(
                params
                    .iter()
                    .map(|(key, value)| (key.as_str(), value.as_str())),
            );
            owner.put("href", Value::string(url.as_str()));
            owner.put(
                "search",
                Value::string(
                    url.query()
                        .map(|query| format!("?{query}"))
                        .unwrap_or_default(),
                ),
            );
        }
    }
}

fn bind_url_params(value: &Value, owner: Value) {
    let params = url_params(value);
    if let Some(object) = value.as_object() {
        object.borrow_mut().payload = Some(Rc::new(UrlParams {
            pairs: params,
            owner: Some(owner),
        }));
    }
}

fn media_query_matches(query: &str, width: f64, height: f64, media_type: &str) -> bool {
    let query = query.trim().to_ascii_lowercase();
    if query.is_empty() {
        return false;
    }
    query.split(',').any(|alternative| {
        let mut matched = true;
        for term in alternative.split("and").map(str::trim) {
            let term = term.trim_matches(|ch| ch == '(' || ch == ')').trim();
            if term == "not all" || term == "not screen" && media_type == "screen" {
                matched = false;
            } else if term == "screen" {
                matched &= media_type == "screen";
            } else if term == "print" {
                matched &= media_type == "print";
            } else if let Some(value) = term
                .strip_prefix("min-width:")
                .and_then(|v| v.trim().strip_suffix("px"))
                .and_then(|v| v.trim().parse::<f64>().ok())
            {
                matched &= width >= value;
            } else if let Some(value) = term
                .strip_prefix("max-width:")
                .and_then(|v| v.trim().strip_suffix("px"))
                .and_then(|v| v.trim().parse::<f64>().ok())
            {
                matched &= width <= value;
            } else if let Some(value) = term
                .strip_prefix("min-height:")
                .and_then(|v| v.trim().strip_suffix("px"))
                .and_then(|v| v.trim().parse::<f64>().ok())
            {
                matched &= height >= value;
            } else if let Some(value) = term
                .strip_prefix("max-height:")
                .and_then(|v| v.trim().strip_suffix("px"))
                .and_then(|v| v.trim().parse::<f64>().ok())
            {
                matched &= height <= value;
            } else if matches!(term, "all" | "not all" | "screen" | "print") {
            } else {
                matched = false;
            }
        }
        matched
    })
}

fn arg(args: &[Value], index: usize) -> Value {
    args.get(index).cloned().unwrap_or(Value::Undefined)
}

fn web_string(value: &Value) -> String {
    match value {
        Value::Undefined => "undefined".to_string(),
        Value::Null => "null".to_string(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) if value.is_nan() => "NaN".to_string(),
        Value::Number(value) if *value == 0.0 => "0".to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(_) => value.text(),
        Value::Object(_) => value.get("toString").text(),
    }
}

fn is_web_iterable(value: &Value) -> bool {
    matches!(
        value.kind().as_str(),
        "Array"
            | "String"
            | "NodeList"
            | "HTMLCollection"
            | "Uint8Array"
            | "Uint8ClampedArray"
            | "Uint16Array"
            | "Uint32Array"
            | "Int8Array"
            | "Int16Array"
            | "Int32Array"
            | "Float32Array"
            | "Float64Array"
    )
}

fn bytes_from_value(value: &Value) -> Vec<u8> {
    if let Some(object) = value.as_object() {
        if let Some(payload) = object.borrow().payload.as_ref() {
            if let Some(bytes) = payload.downcast_ref::<BlobBytes>() {
                return bytes.0.clone();
            }
        }
        if value.kind().ends_with("Array") || value.kind() == "ArrayBuffer" {
            return value
                .items()
                .iter()
                .map(|item| item.number_value().clamp(0.0, 255.0) as u8)
                .collect();
        }
    }
    web_string(value).into_bytes()
}

fn blob_bytes(value: &Value) -> Vec<u8> {
    value
        .as_object()
        .and_then(|object| {
            object
                .borrow()
                .payload
                .as_ref()
                .and_then(|payload| payload.downcast_ref::<BlobBytes>().cloned())
        })
        .map(|bytes| bytes.0)
        .unwrap_or_default()
}

fn make_blob(bytes: Vec<u8>, mime: &str) -> Value {
    let blob = Value::object("Blob");
    blob.put("size", Value::Number(bytes.len() as f64));
    blob.put("type", Value::string(mime.to_ascii_lowercase()));
    if let Some(object) = blob.as_object() {
        let mut object = object.borrow_mut();
        // Keep a host-readable indexed copy as well as the owned byte payload:
        // typed embedder bridges can inspect Blob bytes without JS serialization.
        object.indexed = bytes
            .iter()
            .map(|byte| Value::Number(*byte as f64))
            .collect();
        object.payload = Some(Rc::new(BlobBytes(bytes)));
    }
    blob
}

fn make_uint8(bytes: &[u8]) -> Value {
    let array = Value::object("Uint8Array");
    if let Some(object) = array.as_object() {
        let mut object = object.borrow_mut();
        object.indexed = bytes
            .iter()
            .map(|byte| Value::Number(*byte as f64))
            .collect();
        object.props.insert(
            "length".to_string(),
            Property::data(Value::Number(bytes.len() as f64)),
        );
    }
    array
}

fn make_array_buffer(bytes: &[u8]) -> Value {
    let buffer = Value::object("ArrayBuffer");
    if let Some(object) = buffer.as_object() {
        let mut object = object.borrow_mut();
        object.indexed = bytes
            .iter()
            .map(|byte| Value::Number(*byte as f64))
            .collect();
        object.props.insert(
            "byteLength".to_owned(),
            Property::data(Value::Number(bytes.len() as f64)),
        );
        object.payload = Some(Rc::new(BlobBytes(bytes.to_vec())));
    }
    buffer
}

fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let a = chunk[0];
        let b = *chunk.get(1).unwrap_or(&0);
        let c = *chunk.get(2).unwrap_or(&0);
        output.push(TABLE[(a >> 2) as usize] as char);
        output.push(TABLE[(((a & 3) << 4) | (b >> 4)) as usize] as char);
        output.push(if chunk.len() > 1 {
            TABLE[(((b & 15) << 2) | (c >> 6)) as usize] as char
        } else {
            '='
        });
        output.push(if chunk.len() > 2 {
            TABLE[(c & 63) as usize] as char
        } else {
            '='
        });
    }
    output
}

fn base64_decode(input: &str) -> Result<Vec<u8>> {
    let mut output = Vec::with_capacity(input.len().saturating_mul(3) / 4);
    let mut quartet = [0u8; 4];
    let mut count = 0;
    for byte in input.bytes().filter(|byte| !byte.is_ascii_whitespace()) {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => 64,
            _ => return Err(Error::new("InvalidCharacterError", "Invalid base64 data")),
        };
        quartet[count] = value;
        count += 1;
        if count == 4 {
            if quartet[0] >= 64 || quartet[1] >= 64 {
                return Err(Error::new(
                    "InvalidCharacterError",
                    "Invalid base64 padding",
                ));
            }
            output.push((quartet[0] << 2) | (quartet[1] >> 4));
            if quartet[2] < 64 {
                output.push((quartet[1] << 4) | (quartet[2] >> 2));
                if quartet[3] < 64 {
                    output.push((quartet[2] << 6) | quartet[3]);
                }
            }
            count = 0;
        }
    }
    if count == 2 {
        output.push((quartet[0] << 2) | (quartet[1] >> 4));
    } else if count == 3 {
        output.push((quartet[0] << 2) | (quartet[1] >> 4));
        output.push((quartet[1] << 4) | (quartet[2] >> 2));
    } else if count == 1 {
        return Err(Error::new("InvalidCharacterError", "Invalid base64 length"));
    }
    Ok(output)
}

fn utf8_decode(bytes: &[u8], fatal: bool) -> Result<String> {
    match std::str::from_utf8(bytes) {
        Ok(value) => Ok(value.trim_start_matches('\u{feff}').to_string()),
        Err(_) if fatal => Err(Error::new("TypeError", "Invalid UTF-8 data")),
        Err(_) => Ok(String::from_utf8_lossy(bytes)
            .trim_start_matches('\u{feff}')
            .to_string()),
    }
}

fn encode_uri_component(input: &str, component: bool) -> String {
    let mut output = String::with_capacity(input.len());
    for byte in input.bytes() {
        let allowed = byte.is_ascii_alphanumeric()
            || matches!(
                byte,
                b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')'
            )
            || (!component
                && matches!(
                    byte,
                    b';' | b',' | b'/' | b'?' | b':' | b'@' | b'&' | b'=' | b'+' | b'$' | b'#'
                ));
        if allowed {
            output.push(byte as char);
        } else {
            output.push('%');
            output.push_str(&format!("{byte:02X}"));
        }
    }
    output
}

fn decode_uri_component(input: &str, component: bool) -> Result<String> {
    let bytes = input.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return Err(Error::new("URIError", "Malformed URI sequence"));
            }
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap_or("");
            let value = u8::from_str_radix(hex, 16)
                .map_err(|_| Error::new("URIError", "Malformed URI sequence"))?;
            if !component && b";/?:@&=+$,#".contains(&value) {
                output.extend_from_slice(&bytes[index..index + 3]);
            } else {
                output.push(value);
            }
            index += 3;
        } else {
            output.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(output).map_err(|_| Error::new("URIError", "Malformed UTF-8 sequence"))
}

fn annex_b_escape(input: &str) -> String {
    let mut output = String::new();
    for unit in input.encode_utf16() {
        let allowed = (0x41..=0x5a).contains(&unit)
            || (0x61..=0x7a).contains(&unit)
            || (0x30..=0x39).contains(&unit)
            || matches!(unit, 0x40 | 0x2a | 0x5f | 0x2b | 0x2d | 0x2e | 0x2f);
        if allowed {
            output.push(char::from_u32(unit as u32).unwrap_or('\u{fffd}'));
        } else if unit < 0x100 {
            output.push_str(&format!("%{unit:02X}"));
        } else {
            output.push_str(&format!("%u{unit:04X}"));
        }
    }
    output
}

fn annex_b_unescape(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut units = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 5 < bytes.len() && bytes[index + 1] == b'u' {
            let hex = std::str::from_utf8(&bytes[index + 2..index + 6]).unwrap_or("");
            if let Ok(value) = u16::from_str_radix(hex, 16) {
                units.push(value);
                index += 6;
                continue;
            }
        }
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap_or("");
            if let Ok(value) = u8::from_str_radix(hex, 16) {
                units.push(value as u16);
                index += 3;
                continue;
            }
        }
        let tail = std::str::from_utf8(&bytes[index..]).unwrap_or("\u{fffd}");
        let ch = tail.chars().next().unwrap_or('\u{fffd}');
        let mut pair = [0u16; 2];
        units.extend_from_slice(ch.encode_utf16(&mut pair));
        index += ch.len_utf8();
    }
    String::from_utf16_lossy(&units)
}

fn locale_number(value: f64, options: &Value) -> String {
    if value.is_nan() {
        return "NaN".to_string();
    }
    if value == f64::INFINITY {
        return "\\u{221e}".to_string();
    }
    if value == f64::NEG_INFINITY {
        return "-\\u{221e}".to_string();
    }
    let maximum = options.get("maximumFractionDigits").number_value();
    let minimum = options.get("minimumFractionDigits").number_value();
    let max_digits = if maximum.is_finite() {
        maximum.clamp(0.0, 20.0) as usize
    } else {
        3
    };
    let min_digits = if minimum.is_finite() {
        (minimum.clamp(0.0, 20.0) as usize).min(max_digits)
    } else {
        0
    };
    let mut plain = format!("{value:.max_digits$}");
    while plain.contains('.')
        && plain.ends_with('0')
        && plain.split('.').nth(1).unwrap_or("").len() > min_digits
    {
        plain.pop();
    }
    if plain.ends_with('.') {
        plain.pop();
    }
    let (sign, body) = plain
        .strip_prefix('-')
        .map(|body| ("-", body))
        .unwrap_or(("", plain.as_str()));
    let (integer, fraction) = body.split_once('.').unwrap_or((body, ""));
    let mut grouped = String::with_capacity(integer.len() + integer.len() / 3);
    let use_grouping = !matches!(options.get("useGrouping"), Value::Bool(false));
    for (index, digit) in integer.chars().enumerate() {
        if use_grouping && index != 0 && (integer.len() - index) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    let fraction = if fraction.len() < min_digits {
        format!("{fraction}{}", "0".repeat(min_digits - fraction.len()))
    } else {
        fraction.to_string()
    };
    if fraction.is_empty() {
        format!("{sign}{grouped}")
    } else {
        format!("{sign}{grouped}.{fraction}")
    }
}

impl<B: RuntimeBridge> Guest<B> {
    pub(super) fn install_web(&mut self) {
        for (name, value) in [
            ("Promise", web_fn("Promise")),
            ("Blob", web_fn("Blob")),
            ("TextEncoder", web_fn("TextEncoder")),
            ("TextDecoder", web_fn("TextDecoder")),
            ("AbortController", web_fn("AbortController")),
            ("AbortSignal", web_fn("AbortSignal")),
            ("PerformanceObserver", web_fn("PerformanceObserver")),
            ("Path2D", web_fn("Path2D")),
            ("MessageChannel", web_fn("MessageChannel")),
            ("Intl", web_object("Intl")),
            ("URL", web_fn("URL")),
            ("URLSearchParams", web_fn("URLSearchParams")),
            ("DOMException", web_fn("DOMException")),
            ("matchMedia", web_fn("matchMedia")),
            ("requestIdleCallback", web_fn("requestIdleCallback")),
            ("cancelIdleCallback", web_fn("cancelIdleCallback")),
            ("MutationObserver", web_fn("MutationObserver")),
            ("ResizeObserver", web_fn("ResizeObserver")),
            ("IntersectionObserver", web_fn("IntersectionObserver")),
            ("btoa", web_fn("btoa")),
            ("atob", web_fn("atob")),
            ("escape", web_fn("escape")),
            ("unescape", web_fn("unescape")),
            ("encodeURI", web_fn("encodeURI")),
            ("encodeURIComponent", web_fn("encodeURIComponent")),
            ("decodeURI", web_fn("decodeURI")),
            ("decodeURIComponent", web_fn("decodeURIComponent")),
        ] {
            self.global.put(name, value);
        }
        self.global
            .get("Intl")
            .put("NumberFormat", web_fn("Intl.NumberFormat"));
        self.global
            .get("PerformanceObserver")
            .put("supportedEntryTypes", Value::array(Vec::new()));
        for name in ["DOMException", "AbortSignal", "AbortController"] {
            self.global
                .get(name)
                .put("prototype", Value::object(format!("{name}Prototype")));
        }
        self.global
            .put("customElements", web_object("CustomElementRegistry"));
        self.global
            .put_hidden("__web_object_urls", web_object("ObjectURLRegistry"));
        self.global.put_hidden("__web_url_next", Value::Number(1.0));
        self.global
            .put_hidden("__web_custom_pending", web_object("Object"));
        self.global
            .put_hidden("__web_promise_callbacks", web_object("Object"));
        self.global
            .put_hidden("__web_promise_callback_next", Value::Number(1.0));
        self.global
            .put_hidden("__web_ports", Value::array(Vec::new()));
        self.global
            .put_hidden("__web_aggregates", web_object("Object"));
        self.global
            .put_hidden("__web_finally", web_object("Object"));
        self.global
            .put_hidden("__web_idle_callbacks", web_object("Object"));
        self.global
            .put_hidden("__web_observers", Value::array(Vec::new()));
        self.global
            .put_hidden("__web_started_at", Value::Number(0.0));
        self.global.put("performance", web_object("Performance"));
        self.global.put_hidden(
            "__web_time_origin",
            Value::Number(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0.0, |duration| duration.as_secs_f64() * 1000.0),
            ),
        );
        self.global
            .get("performance")
            .put("timeOrigin", self.global.get("__web_time_origin"));
        if let Some(object) = self.global.get("performance").as_object() {
            object.borrow_mut().payload = Some(Rc::new(Instant::now()));
        }
    }

    pub(super) fn web_get(
        &mut self,
        _doc: &mut blitz_dom::BaseDocument,
        receiver: &Value,
        key: &str,
    ) -> Result<Option<Value>> {
        let kind = receiver.kind();
        if kind == "Global" && matches!(key, "scrollX" | "scrollY" | "pageXOffset" | "pageYOffset")
        {
            let scroll = _doc.viewport_scroll();
            return Ok(Some(Value::Number(
                if matches!(key, "scrollX" | "pageXOffset") {
                    scroll.x
                } else {
                    scroll.y
                },
            )));
        }
        if kind == "PerformanceObserver" && matches!(key, "observe" | "disconnect" | "takeRecords")
        {
            return Ok(Some(web_fn(&format!("PerformanceObserver.{key}"))));
        }
        if kind == "Path2D"
            && matches!(
                key,
                "moveTo"
                    | "lineTo"
                    | "rect"
                    | "closePath"
                    | "bezierCurveTo"
                    | "quadraticCurveTo"
                    | "arc"
                    | "ellipse"
                    | "addPath"
            )
        {
            return Ok(Some(web_fn(&format!("Path2D.{key}"))));
        }
        if kind == "Native"
            && receiver.get("__native").text() == "Web.AbortSignal"
            && key == "abort"
        {
            return Ok(Some(web_fn("AbortSignal.abort")));
        }
        if kind == "URL" && key == "searchParams" {
            let params = web_object("URLSearchParams");
            set_url_params(
                &params,
                parse_url_params(&receiver.get("search").text()),
                None,
            );
            bind_url_params(&params, receiver.clone());
            return Ok(Some(params));
        }
        let name = match kind.as_str() {
            "Promise" => match key {
                "then" => Some("Promise.then"),
                "catch" => Some("Promise.catch"),
                "finally" => Some("Promise.finally"),
                _ => None,
            },
            "Native" if receiver.get("__native").text() == "Web.Promise" => match key {
                "resolve" => Some("Promise.resolve"),
                "reject" => Some("Promise.reject"),
                "all" => Some("Promise.all"),
                "race" => Some("Promise.race"),
                _ => None,
            },
            "Native" if receiver.get("__native").text() == "Web.URL" => match key {
                "createObjectURL" => Some("URL.createObjectURL"),
                "revokeObjectURL" => Some("URL.revokeObjectURL"),
                _ => None,
            },
            "Blob" => match key {
                "text" => Some("Blob.text"),
                "arrayBuffer" => Some("Blob.arrayBuffer"),
                "slice" => Some("Blob.slice"),
                _ => None,
            },
            "TextEncoder" => (key == "encode").then_some("TextEncoder.encode"),
            "TextDecoder" => (key == "decode").then_some("TextDecoder.decode"),
            "AbortController" => (key == "abort").then_some("AbortController.abort"),
            "MessagePort" => match key {
                "postMessage" => Some("MessagePort.postMessage"),
                "start" => Some("MessagePort.start"),
                "close" => Some("MessagePort.close"),
                _ => None,
            },
            "CustomElementRegistry" => match key {
                "define" => Some("CustomElements.define"),
                "get" => Some("CustomElements.get"),
                "whenDefined" => Some("CustomElements.whenDefined"),
                "upgrade" => Some("CustomElements.upgrade"),
                _ => None,
            },
            "URL" => match key {
                "createObjectURL" => Some("URL.createObjectURL"),
                "revokeObjectURL" => Some("URL.revokeObjectURL"),
                _ => None,
            },
            "Intl" if key == "NumberFormat" => Some("Intl.NumberFormat"),
            "NumberFormat" if key == "format" => Some("Intl.NumberFormat.format"),
            "NumberFormat" if key == "resolvedOptions" => Some("Intl.NumberFormat.resolvedOptions"),
            "AbortSignal" if key == "throwIfAborted" => Some("AbortSignal.throwIfAborted"),
            "DOMException" if key == "toString" => Some("DOMException.toString"),
            "URLSearchParams" => match key {
                "append" => Some("URLSearchParams.append"),
                "delete" => Some("URLSearchParams.delete"),
                "get" => Some("URLSearchParams.get"),
                "getAll" => Some("URLSearchParams.getAll"),
                "has" => Some("URLSearchParams.has"),
                "set" => Some("URLSearchParams.set"),
                "sort" => Some("URLSearchParams.sort"),
                "toString" => Some("URLSearchParams.toString"),
                _ => None,
            },
            "Native" if receiver.get("__native").text() == "Web.URLSearchParams" => {
                Some("URLSearchParams.constructor")
            }
            "Native" if receiver.get("__native").text() == "Web.DOMException" => {
                Some("DOMException.constructor")
            }
            "Performance"
                if matches!(
                    key,
                    "now"
                        | "mark"
                        | "getEntries"
                        | "getEntriesByType"
                        | "getEntriesByName"
                        | "clearMarks"
                ) =>
            {
                return Ok(Some(web_fn(&format!("Performance.{key}"))));
            }
            "Performance" if key == "timeOrigin" => None,
            "MutationObserver" | "ResizeObserver" | "IntersectionObserver" => match key {
                "observe" => Some("Observer.observe"),
                "unobserve" => Some("Observer.unobserve"),
                "disconnect" => Some("Observer.disconnect"),
                "takeRecords" => Some("Observer.takeRecords"),
                _ => None,
            },
            _ => None,
        };
        Ok(name.map(web_fn))
    }

    pub(super) fn web_set(
        &mut self,
        _doc: &mut blitz_dom::BaseDocument,
        receiver: &Value,
        key: &str,
        value: Value,
    ) -> Result<bool> {
        if receiver.kind() == "MessagePort" && key == "onmessage" {
            receiver.put(key, value);
            if let Some(state) = port_payload(receiver) {
                state.borrow_mut().started = true;
            }
            self.drain_message_port(receiver);
            return Ok(true);
        }
        if receiver.kind() == "NumberFormat" && key == "format" {
            receiver.put_hidden("__format_bound", value);
            return Ok(true);
        }
        Ok(false)
    }

    pub(super) fn web_call(
        &mut self,
        doc: &mut blitz_dom::BaseDocument,
        name: &str,
        this: Value,
        args: Vec<Value>,
        construct: bool,
    ) -> Result<Option<Value>> {
        let value = match name {
            "Web.AnimationCallback" => {
                let timestamp = self
                    .web_call(
                        doc,
                        "Web.Performance.now",
                        self.global.get("performance"),
                        Vec::new(),
                        false,
                    )?
                    .unwrap_or(Value::Number(0.0));
                self.call(
                    doc,
                    this.get("callback"),
                    self.global.clone(),
                    vec![timestamp],
                    false,
                )?;
                Some(Value::Undefined)
            }
            "Web.Performance.mark" => {
                let start = self
                    .web_call(doc, "Web.Performance.now", this.clone(), Vec::new(), false)?
                    .unwrap_or(Value::Number(0.0));
                let entry = web_object("PerformanceMark");
                entry.put("name", arg(&args, 0));
                entry.put("entryType", Value::string("mark"));
                entry.put("startTime", start);
                entry.put("duration", Value::Number(0.0));
                let mut entries = this.get("entries").items();
                entries.push(entry.clone());
                if entries.len() > MAX_WEB_ITEMS {
                    return Err(Error::new(
                        "QuotaExceededError",
                        "performance timeline limit exceeded",
                    ));
                }
                this.put("entries", Value::array(entries));
                Some(entry)
            }
            "Web.Performance.getEntries"
            | "Web.Performance.getEntriesByType"
            | "Web.Performance.getEntriesByName" => {
                let entries = this
                    .get("entries")
                    .items()
                    .into_iter()
                    .filter(|entry| match name {
                        "Web.Performance.getEntriesByType" => {
                            entry.get("entryType").same(&arg(&args, 0))
                        }
                        "Web.Performance.getEntriesByName" => {
                            entry.get("name").same(&arg(&args, 0))
                                && (args.len() < 2 || entry.get("entryType").same(&arg(&args, 1)))
                        }
                        _ => true,
                    })
                    .collect();
                Some(Value::array(entries))
            }
            "Web.Performance.clearMarks" => {
                let entries = this
                    .get("entries")
                    .items()
                    .into_iter()
                    .filter(|entry| {
                        entry.get("entryType").text() != "mark"
                            || (!args.is_empty() && !entry.get("name").same(&arg(&args, 0)))
                    })
                    .collect();
                this.put("entries", Value::array(entries));
                Some(Value::Undefined)
            }
            "Web.Path2D" if construct => {
                let initial = arg(&args, 0);
                if !matches!(initial, Value::Undefined) && initial.kind() != "Path2D" {
                    return Err(Error::new(
                        "NotSupportedError",
                        "Path2D admits path objects and command methods",
                    ));
                }
                let path = web_object("Path2D");
                if let Some(object) = path.as_object() {
                    object.borrow_mut().indexed = initial.items();
                }
                Some(path)
            }
            operation if operation.starts_with("Web.Path2D.") => {
                let operation = operation.trim_start_matches("Web.Path2D.");
                let object = this
                    .as_object()
                    .ok_or_else(|| Error::type_error("invalid path receiver"))?;
                if object.borrow().indexed.len() >= MAX_WEB_ITEMS {
                    return Err(Error::new(
                        "QuotaExceededError",
                        "path command limit exceeded",
                    ));
                }
                if operation == "addPath" {
                    if args
                        .get(1)
                        .is_some_and(|value| !matches!(value, Value::Undefined))
                    {
                        return Err(Error::new(
                            "NotSupportedError",
                            "transformed path composition is not admitted",
                        ));
                    }
                    object.borrow_mut().indexed.extend(arg(&args, 0).items());
                } else {
                    let command = web_object("PathCommand");
                    command.put("op", Value::string(operation));
                    command.put("n", Value::array(args));
                    object.borrow_mut().indexed.push(command);
                }
                Some(Value::Undefined)
            }
            "Web.PerformanceObserver" if construct => {
                if !matches!(arg(&args, 0).kind().as_str(), "Function" | "Native") {
                    return Err(Error::type_error("PerformanceObserver requires a callback"));
                }
                let observer = web_object("PerformanceObserver");
                observer.put("callback", arg(&args, 0));
                observer.put("records", Value::array(Vec::new()));
                Some(observer)
            }
            "Web.PerformanceObserver.observe" => {
                if arg(&args, 0).kind() != "Object" {
                    return Err(Error::type_error(
                        "performance observer options are required",
                    ));
                }
                Some(Value::Undefined)
            }
            "Web.PerformanceObserver.disconnect" => {
                this.put("records", Value::array(Vec::new()));
                Some(Value::Undefined)
            }
            "Web.PerformanceObserver.takeRecords" => {
                let records = this.get("records");
                this.put("records", Value::array(Vec::new()));
                Some(records)
            }
            "Web.AbortSignal.abort" => {
                let signal = web_object("AbortSignal");
                signal.put("aborted", Value::Bool(true));
                let reason = if args.is_empty() {
                    self.web_call(
                        doc,
                        "Web.DOMException",
                        Value::Undefined,
                        vec![
                            Value::string("The operation was aborted"),
                            Value::string("AbortError"),
                        ],
                        true,
                    )?
                    .unwrap_or(Value::Undefined)
                } else {
                    arg(&args, 0)
                };
                signal.put("reason", reason);
                if let Some(object) = signal.as_object() {
                    object.borrow_mut().prototype =
                        Some(self.global.get("AbortSignal").get("prototype"));
                }
                Some(signal)
            }
            "Web.HostResolvePromise" => {
                self.resolve_promise(doc, &arg(&args, 0), arg(&args, 1), arg(&args, 2).truthy())?;
                Some(Value::Undefined)
            }
            "Web.Promise" if construct => {
                if !matches!(arg(&args, 0).kind().as_str(), "Function" | "Native") {
                    return Err(Error::type_error("Promise executor must be callable"));
                }
                let promise = Value::pending_promise();
                if arg(&args, 0).kind() != "Undefined" {
                    let id = self
                        .global
                        .get("__web_promise_callback_next")
                        .number_value() as u64;
                    self.global.put(
                        "__web_promise_callback_next",
                        Value::Number(id.saturating_add(1) as f64),
                    );
                    let key = id.to_string();
                    self.global
                        .get("__web_promise_callbacks")
                        .put(&key, promise.clone());
                    let resolve = web_fn(&format!("PromiseResolve.{id}"));
                    let reject = web_fn(&format!("PromiseReject.{id}"));
                    if let Err(error) = self.call(
                        doc,
                        arg(&args, 0),
                        Value::Undefined,
                        vec![resolve, reject],
                        false,
                    ) {
                        self.settle_promise(&promise, Err(error));
                    }
                }
                Some(promise)
            }
            callback if callback.starts_with("Web.IdleCallback.") => {
                let id = callback.trim_start_matches("Web.IdleCallback.");
                let handler = self.global.get("__web_idle_callbacks").get(id);
                self.global
                    .get("__web_idle_callbacks")
                    .put(id, Value::Undefined);
                if matches!(handler, Value::Undefined) {
                    return Ok(Some(Value::Undefined));
                }
                let deadline = web_object("IdleDeadline");
                deadline.put("didTimeout", Value::Bool(false));
                deadline.put("timeRemaining", web_fn("IdleDeadline.timeRemaining"));
                if let Some(object) = deadline.as_object() {
                    object.borrow_mut().payload = Some(Rc::new(Instant::now()));
                }
                self.call(doc, handler, Value::Undefined, vec![deadline], false)?;
                Some(Value::Undefined)
            }
            "Web.IdleDeadline.timeRemaining" => {
                let elapsed = this
                    .as_object()
                    .and_then(|object| {
                        object
                            .borrow()
                            .payload
                            .as_ref()
                            .and_then(|payload| payload.downcast_ref::<Instant>().copied())
                    })
                    .map_or(50.0, |start| start.elapsed().as_secs_f64() * 1000.0);
                Some(Value::Number((50.0 - elapsed).max(0.0)))
            }
            "Web.requestIdleCallback" => {
                let id = self.next_web_id();
                let callback = web_fn(&format!("IdleCallback.{id}"));
                self.global
                    .get("__web_idle_callbacks")
                    .put(&id.to_string(), arg(&args, 0));
                self.queue(callback, Value::Undefined, Vec::new());
                Some(Value::Number(id as f64))
            }
            "Web.cancelIdleCallback" => {
                let id = web_string(&arg(&args, 0));
                self.global
                    .get("__web_idle_callbacks")
                    .put(&id, Value::Undefined);
                self.jobs.retain(|job| {
                    job.callback.get("__native").text() != format!("Web.IdleCallback.{id}")
                });
                Some(Value::Undefined)
            }
            "Web.Performance.now" => {
                let elapsed = this
                    .as_object()
                    .and_then(|object| {
                        object
                            .borrow()
                            .payload
                            .as_ref()
                            .and_then(|payload| payload.downcast_ref::<Instant>().copied())
                    })
                    .map_or(0.0, |start| start.elapsed().as_secs_f64() * 1000.0);
                Some(Value::Number(elapsed))
            }
            "Web.matchMedia" => {
                let query = web_string(&arg(&args, 0));
                let viewport = doc.get_viewport();
                let scale = viewport.scale_f64();
                let width = viewport.window_size.0 as f64 / scale;
                let height = viewport.window_size.1 as f64 / scale;
                let media_type = if *doc.media_type() == blitz_dom::MediaType::screen() {
                    "screen"
                } else {
                    "print"
                };
                let result = web_object("MediaQueryList");
                result.put("media", Value::string(&query));
                result.put(
                    "matches",
                    Value::Bool(media_query_matches(&query, width, height, media_type)),
                );
                Some(result)
            }
            "Web.URLSearchParams" if construct => {
                let params = web_object("URLSearchParams");
                let initial = arg(&args, 0);
                let pairs = if initial.kind() == "Undefined" {
                    Vec::new()
                } else if initial.kind() == "String" {
                    parse_url_params(&initial.text())
                } else if initial.kind() == "Array" {
                    initial
                        .items()
                        .into_iter()
                        .filter_map(|pair| {
                            let pair = pair.items();
                            (pair.len() >= 2).then(|| (pair[0].text(), pair[1].text()))
                        })
                        .collect()
                } else {
                    Vec::new()
                };
                set_url_params(&params, pairs, None);
                Some(params)
            }
            "Web.URLSearchParams.append"
            | "Web.URLSearchParams.delete"
            | "Web.URLSearchParams.set"
            | "Web.URLSearchParams.sort" => {
                let mut pairs = url_params(&this);
                let key = web_string(&arg(&args, 0));
                match name {
                    "Web.URLSearchParams.append" => pairs.push((key, web_string(&arg(&args, 1)))),
                    "Web.URLSearchParams.delete" => pairs.retain(|(existing, _)| existing != &key),
                    "Web.URLSearchParams.set" => {
                        let value = web_string(&arg(&args, 1));
                        let mut inserted = false;
                        pairs.retain_mut(|(existing, current)| {
                            if existing == &key {
                                if inserted {
                                    false
                                } else {
                                    *current = value.clone();
                                    inserted = true;
                                    true
                                }
                            } else {
                                true
                            }
                        });
                        if !inserted {
                            pairs.push((key, value));
                        }
                    }
                    "Web.URLSearchParams.sort" => pairs.sort_by(|left, right| left.0.cmp(&right.0)),
                    _ => {}
                }
                let owner = this.as_object().and_then(|object| {
                    object.borrow().payload.as_ref().and_then(|payload| {
                        payload
                            .downcast_ref::<UrlParams>()
                            .and_then(|params| params.owner.clone())
                    })
                });
                set_url_params(&this, pairs, owner);
                Some(Value::Undefined)
            }
            "Web.URLSearchParams.get" => Some(
                url_params(&this)
                    .iter()
                    .find(|(key, _)| key == &web_string(&arg(&args, 0)))
                    .map_or(Value::Null, |(_, value)| Value::string(value)),
            ),
            "Web.URLSearchParams.getAll" => Some(Value::array(
                url_params(&this)
                    .iter()
                    .filter(|(key, _)| key == &web_string(&arg(&args, 0)))
                    .map(|(_, value)| Value::string(value))
                    .collect(),
            )),
            "Web.URLSearchParams.has" => Some(Value::Bool(
                url_params(&this)
                    .iter()
                    .any(|(key, _)| key == &web_string(&arg(&args, 0))),
            )),
            "Web.URLSearchParams.toString" => {
                Some(Value::string(url_param_string(&url_params(&this))))
            }
            "Web.DOMException" if construct => {
                let message = args.first().map(web_string).unwrap_or_default();
                let name = args
                    .get(1)
                    .map(web_string)
                    .filter(|name| !name.is_empty())
                    .unwrap_or_else(|| "Error".to_owned());
                let exception = web_object("DOMException");
                if let Some(object) = exception.as_object() {
                    object.borrow_mut().prototype =
                        Some(self.global.get("DOMException").get("prototype"));
                }
                exception.put("name", Value::string(name));
                exception.put("message", Value::string(message));
                exception.put(
                    "code",
                    Value::Number(match exception.get("name").text().as_str() {
                        "IndexSizeError" => 1.0,
                        "HierarchyRequestError" => 3.0,
                        "WrongDocumentError" => 4.0,
                        "InvalidCharacterError" => 5.0,
                        "NoModificationAllowedError" => 7.0,
                        "NotFoundError" => 8.0,
                        "NotSupportedError" => 9.0,
                        "InvalidStateError" => 11.0,
                        "SyntaxError" => 12.0,
                        "InvalidModificationError" => 13.0,
                        "NamespaceError" => 14.0,
                        "InvalidAccessError" => 15.0,
                        "TypeMismatchError" => 17.0,
                        "SecurityError" => 18.0,
                        "NetworkError" => 19.0,
                        "AbortError" => 20.0,
                        "URLMismatchError" => 21.0,
                        "QuotaExceededError" => 22.0,
                        "TimeoutError" => 23.0,
                        "InvalidNodeTypeError" => 24.0,
                        "DataCloneError" => 25.0,
                        _ => 0.0,
                    }),
                );
                Some(exception)
            }
            "Web.DOMException.toString" => {
                Some(Value::string(if this.get("message").text().is_empty() {
                    this.get("name").text()
                } else {
                    format!(
                        "{}: {}",
                        this.get("name").text(),
                        this.get("message").text()
                    )
                }))
            }
            "Web.MutationObserver" | "Web.ResizeObserver" | "Web.IntersectionObserver"
                if construct =>
            {
                if !matches!(arg(&args, 0).kind().as_str(), "Function" | "Native") {
                    return Err(Error::type_error("observer callback must be callable"));
                }
                let observer = web_object(name.trim_start_matches("Web."));
                observer.put("callback", arg(&args, 0));
                observer.put("observed", Value::array(Vec::new()));
                observer.put("records", Value::array(Vec::new()));
                let mut all = self.global.get("__web_observers").items();
                all.push(observer.clone());
                self.global.put_hidden("__web_observers", Value::array(all));
                Some(observer)
            }
            "Web.Observer.observe" => {
                let kind = this.kind();
                let target = arg(&args, 0);
                if observer_node_id(&target).is_none() {
                    return Err(Error::type_error("observer target must be a live DOM node"));
                }
                let options = arg(&args, 1);
                if kind == "MutationObserver" && options.get("subtree").truthy() {
                    return Err(Error::new(
                        "NotSupportedError",
                        "MutationObserver observes the target node only",
                    ));
                }
                let attribute_filter = options.get("attributeFilter").items();
                let watch_attributes = options.get("attributes").truthy()
                    || options.get("attributeOldValue").truthy()
                    || !attribute_filter.is_empty();
                let watch_character_data = options.get("characterData").truthy()
                    || options.get("characterDataOldValue").truthy();
                if kind == "MutationObserver"
                    && !watch_attributes
                    && !options.get("childList").truthy()
                    && !watch_character_data
                {
                    return Err(Error::type_error(
                        "MutationObserver.observe requires an enabled mutation type",
                    ));
                }
                let root_margin = options.get("rootMargin").text();
                if kind == "ResizeObserver"
                    && !matches!(
                        options.get("box").text().as_str(),
                        "undefined" | "content-box"
                    )
                {
                    return Err(Error::new(
                        "NotSupportedError",
                        "ResizeObserver supports content-box observation only",
                    ));
                }
                let thresholds = options.get("threshold");
                if kind == "IntersectionObserver"
                    && ((is_web_iterable(&thresholds)
                        && thresholds
                            .items()
                            .iter()
                            .any(|value| value.number_value() != 0.0))
                        || (!is_web_iterable(&thresholds)
                            && thresholds.kind() != "Undefined"
                            && thresholds.number_value() != 0.0))
                {
                    return Err(Error::new(
                        "NotSupportedError",
                        "IntersectionObserver supports threshold 0 only",
                    ));
                }
                if kind == "IntersectionObserver"
                    && (options.get("root").truthy()
                        || !matches!(
                            root_margin.as_str(),
                            "undefined" | "0px" | "0px 0px" | "0px 0px 0px" | "0px 0px 0px 0px"
                        ))
                {
                    return Err(Error::new(
                        "NotSupportedError",
                        "IntersectionObserver supports the document viewport root and zero root margin",
                    ));
                }
                let registration = web_object("ObserverTarget");
                registration.put("target", target);
                registration.put("attributes", Value::Bool(watch_attributes));
                registration.put("childList", Value::Bool(options.get("childList").truthy()));
                registration.put("characterData", Value::Bool(watch_character_data));
                registration.put(
                    "attributeOldValue",
                    Value::Bool(options.get("attributeOldValue").truthy()),
                );
                registration.put(
                    "characterDataOldValue",
                    Value::Bool(options.get("characterDataOldValue").truthy()),
                );
                registration.put("attributeFilter", options.get("attributeFilter"));
                registration.put("oldAttributes", web_object("Object"));
                registration.put("oldText", Value::string(""));
                registration.put("oldChildren", Value::array(Vec::new()));
                registration.put("oldRect", Value::array(Vec::new()));
                if let Some(id) = observer_node_id(&registration.get("target")) {
                    if let Some(node) = doc.get_node(id) {
                        registration.put("oldAttributes", attributes_snapshot(node));
                        registration.put(
                            "oldText",
                            Value::string(
                                node.text_data()
                                    .map_or("".to_owned(), |text| text.content.clone()),
                            ),
                        );
                        registration.put(
                            "oldChildren",
                            Value::array(
                                node.children
                                    .iter()
                                    .map(|child| Value::Number(child.as_u64() as f64))
                                    .collect(),
                            ),
                        );
                        if kind == "MutationObserver" {
                            registration.put("oldRect", node_rect(node));
                        }
                    }
                }
                let mut observed = this.get("observed").items();
                observed.retain(|entry| !entry.get("target").same(&registration.get("target")));
                observed.push(registration);
                this.put("observed", Value::array(observed));
                Some(Value::Undefined)
            }
            "Web.Observer.unobserve" => {
                let target = arg(&args, 0);
                let observed = this
                    .get("observed")
                    .items()
                    .into_iter()
                    .filter(|entry| !entry.get("target").same(&target))
                    .collect();
                this.put("observed", Value::array(observed));
                Some(Value::Undefined)
            }
            "Web.Observer.disconnect" => {
                this.put("observed", Value::array(Vec::new()));
                this.put("records", Value::array(Vec::new()));
                Some(Value::Undefined)
            }
            "Web.Observer.takeRecords" => {
                let records = this.get("records").items();
                this.put("records", Value::array(Vec::new()));
                Some(Value::array(records))
            }
            callback if callback.starts_with("Web.PromiseResolve.") => {
                let id = callback.trim_start_matches("Web.PromiseResolve.");
                let registry = self.global.get("__web_promise_callbacks");
                let promise = registry.get(id);
                registry.put(id, Value::Undefined);
                self.resolve_promise(doc, &promise, arg(&args, 0), false)?;
                Some(Value::Undefined)
            }
            callback if callback.starts_with("Web.PromiseReject.") => {
                let id = callback.trim_start_matches("Web.PromiseReject.");
                let registry = self.global.get("__web_promise_callbacks");
                let promise = registry.get(id);
                registry.put(id, Value::Undefined);
                self.resolve_promise(doc, &promise, arg(&args, 0), true)?;
                Some(Value::Undefined)
            }
            "Web.Promise.resolve" => {
                if arg(&args, 0).kind() == "Promise" {
                    Some(arg(&args, 0))
                } else {
                    let promise = Value::pending_promise();
                    self.resolve_promise(doc, &promise, arg(&args, 0), false)?;
                    Some(promise)
                }
            }
            "Web.Promise.reject" => {
                let promise = Value::pending_promise();
                self.resolve_promise(doc, &promise, arg(&args, 0), true)?;
                Some(promise)
            }
            "Web.Promise.all" => {
                let iterable = arg(&args, 0);
                if !is_web_iterable(&iterable) {
                    return Err(Error::type_error("Promise.all input is not iterable"));
                }
                let values = iterable.items();
                if values.len() > MAX_WEB_ITEMS {
                    return Err(Error::new("RangeError", "Too many promise values"));
                }
                let promise = Value::pending_promise();
                let id = self.next_web_id();
                let aggregate = web_object("PromiseAggregate");
                aggregate.put_hidden("promise", promise.clone());
                aggregate.put_hidden("values", Value::array(vec![Value::Undefined; values.len()]));
                aggregate.put_hidden("remaining", Value::Number(values.len() as f64));
                self.global
                    .get("__web_aggregates")
                    .put(&id.to_string(), aggregate);
                if values.is_empty() {
                    self.settle_promise(&promise, Ok(Value::array(Vec::new())));
                }
                for (index, value) in values.into_iter().enumerate() {
                    self.attach_all_item(doc, id, index, value)?;
                }
                Some(promise)
            }
            "Web.Promise.race" => {
                let iterable = arg(&args, 0);
                if !is_web_iterable(&iterable) {
                    return Err(Error::type_error("Promise.race input is not iterable"));
                }
                let values = iterable.items();
                let promise = Value::pending_promise();
                let id = self.next_web_id();
                self.global
                    .get("__web_aggregates")
                    .put(&id.to_string(), promise.clone());
                for value in values {
                    self.attach_race_item(doc, id, value)?;
                }
                Some(promise)
            }
            "Web.Promise.then" | "Web.Promise.catch" | "Web.Promise.finally" => {
                let fulfilled = if name.ends_with("catch") {
                    Value::Undefined
                } else {
                    arg(&args, 0)
                };
                let rejected = if name.ends_with("catch") {
                    arg(&args, 0)
                } else {
                    arg(&args, 1)
                };
                let next = Value::pending_promise();
                let reaction = web_object("PromiseReaction");
                reaction.put_hidden("fulfilled", fulfilled);
                reaction.put_hidden("rejected", rejected);
                reaction.put_hidden("next", next.clone());
                if name.ends_with("finally") {
                    reaction.put_hidden("finally", Value::Bool(true));
                }
                this.as_object()
                    .ok_or_else(|| Error::type_error("Promise receiver is not an object"))?
                    .borrow_mut()
                    .indexed
                    .push(reaction);
                self.schedule_promise_reactions(&this);
                Some(next)
            }
            "Web.PromiseReaction" => {
                let reaction = arg(&args, 1);
                let promise = reaction.get("next");
                let rejected = arg(&args, 2).truthy();
                let input = arg(&args, 3);
                let finalizer = reaction.get("finally").truthy();
                let handler = if finalizer {
                    reaction.get("fulfilled")
                } else {
                    reaction.get(if rejected { "rejected" } else { "fulfilled" })
                };
                if finalizer && handler.kind() != "Undefined" {
                    match self.call(doc, handler, Value::Undefined, Vec::new(), false) {
                        Ok(value) if value.kind() == "Promise" => {
                            match value.get("state").number_value() as u8 {
                                1 => self.resolve_promise(doc, &promise, input, rejected)?,
                                2 => {
                                    self.resolve_promise(doc, &promise, value.get("value"), true)?
                                }
                                _ => {
                                    let id = self.next_web_id();
                                    let pending = web_object("PromiseFinallyContinuation");
                                    pending.put_hidden("promise", promise.clone());
                                    pending.put_hidden("value", input);
                                    pending.put_hidden("rejected", Value::Bool(rejected));
                                    self.global
                                        .get("__web_finally")
                                        .put(&id.to_string(), pending);
                                    let reaction = web_object("PromiseReaction");
                                    reaction.put_hidden(
                                        "fulfilled",
                                        web_fn(&format!("PromiseFinallyDone.{id}")),
                                    );
                                    reaction.put_hidden(
                                        "rejected",
                                        web_fn(&format!("PromiseFinallyError.{id}")),
                                    );
                                    reaction.put_hidden("next", Value::pending_promise());
                                    value
                                        .as_object()
                                        .ok_or_else(|| Error::type_error("invalid promise value"))?
                                        .borrow_mut()
                                        .indexed
                                        .push(reaction);
                                    self.schedule_promise_reactions(&value);
                                }
                            }
                        }
                        Ok(_) => self.resolve_promise(doc, &promise, input, rejected)?,
                        Err(error) => self.resolve_promise(doc, &promise, error.value(), true)?,
                    }
                    return Ok(Some(Value::Undefined));
                }
                let output = if handler.kind() == "Undefined" {
                    if rejected {
                        Err(Error::thrown(input))
                    } else {
                        Ok(input)
                    }
                } else {
                    match self.call(doc, handler, Value::Undefined, vec![input.clone()], false) {
                        Ok(value) if value.same(&reaction.get("next")) => {
                            Err(Error::type_error("Promise cannot resolve to itself"))
                        }
                        Ok(value) => Ok(value),
                        Err(error) => Err(error),
                    }
                };
                match output {
                    Ok(value) => self.resolve_promise(doc, &promise, value, false)?,
                    Err(error) => self.resolve_promise(doc, &promise, error.value(), true)?,
                }
                Some(Value::Undefined)
            }
            callback if callback.starts_with("Web.PromiseAdoptResolve.") => {
                let id = callback.trim_start_matches("Web.PromiseAdoptResolve.");
                let promise = self.global.get("__web_promise_callbacks").get(id);
                self.global
                    .get("__web_promise_callbacks")
                    .put(id, Value::Undefined);
                self.resolve_promise(doc, &promise, arg(&args, 0), false)?;
                Some(Value::Undefined)
            }
            callback if callback.starts_with("Web.PromiseAdoptReject.") => {
                let id = callback.trim_start_matches("Web.PromiseAdoptReject.");
                let promise = self.global.get("__web_promise_callbacks").get(id);
                self.global
                    .get("__web_promise_callbacks")
                    .put(id, Value::Undefined);
                self.resolve_promise(doc, &promise, arg(&args, 0), true)?;
                Some(Value::Undefined)
            }
            callback if callback.starts_with("Web.PromiseFinallyDone.") => {
                let id = callback.trim_start_matches("Web.PromiseFinallyDone.");
                let continuation = self.global.get("__web_finally").get(id);
                self.global.get("__web_finally").put(id, Value::Undefined);
                self.resolve_promise(
                    doc,
                    &continuation.get("promise"),
                    continuation.get("value"),
                    continuation.get("rejected").truthy(),
                )?;
                Some(Value::Undefined)
            }
            callback if callback.starts_with("Web.PromiseFinallyError.") => {
                let id = callback.trim_start_matches("Web.PromiseFinallyError.");
                let continuation = self.global.get("__web_finally").get(id);
                self.global.get("__web_finally").put(id, Value::Undefined);
                self.resolve_promise(doc, &continuation.get("promise"), arg(&args, 0), true)?;
                Some(Value::Undefined)
            }
            callback if callback.starts_with("Web.PromiseAllItem.") => {
                let (id, index) = parse_pair_id(callback, "Web.PromiseAllItem.")?;
                let aggregate = self.global.get("__web_aggregates").get(&id.to_string());
                if aggregate.kind() == "PromiseAggregate"
                    && aggregate.get("remaining").number_value() > 0.0
                {
                    let values = aggregate.get("values");
                    values.put(&index.to_string(), arg(&args, 0));
                    let remaining = aggregate.get("remaining").number_value() - 1.0;
                    aggregate.put_hidden("remaining", Value::Number(remaining));
                    if remaining == 0.0 {
                        let promise = aggregate.get("promise");
                        let result = Value::array(values.items());
                        self.resolve_promise(doc, &promise, result, false)?;
                        self.global
                            .get("__web_aggregates")
                            .put(&id.to_string(), Value::Undefined);
                    }
                }
                Some(Value::Undefined)
            }
            callback if callback.starts_with("Web.PromiseAllReject.") => {
                let id = callback.trim_start_matches("Web.PromiseAllReject.");
                let aggregate = self.global.get("__web_aggregates").get(id);
                let promise = if aggregate.kind() == "PromiseAggregate" {
                    aggregate.get("promise")
                } else {
                    aggregate
                };
                self.global
                    .get("__web_aggregates")
                    .put(id, Value::Undefined);
                self.resolve_promise(doc, &promise, arg(&args, 0), true)?;
                Some(Value::Undefined)
            }
            callback if callback.starts_with("Web.PromiseRace.") => {
                let id = callback.trim_start_matches("Web.PromiseRace.");
                let promise = self.global.get("__web_aggregates").get(id);
                self.global
                    .get("__web_aggregates")
                    .put(id, Value::Undefined);
                self.resolve_promise(doc, &promise, arg(&args, 0), false)?;
                Some(Value::Undefined)
            }
            callback if callback.starts_with("Web.PromiseRaceReject.") => {
                let id = callback.trim_start_matches("Web.PromiseRaceReject.");
                let promise = self.global.get("__web_aggregates").get(id);
                self.global
                    .get("__web_aggregates")
                    .put(id, Value::Undefined);
                self.resolve_promise(doc, &promise, arg(&args, 0), true)?;
                Some(Value::Undefined)
            }
            "Web.Blob" if construct => {
                let chunks = arg(&args, 0).items();
                let options = arg(&args, 1);
                let mut bytes = Vec::new();
                for chunk in chunks {
                    let next = bytes_from_value(&chunk);
                    if next.len() > MAX_BLOB_BYTES.saturating_sub(bytes.len()) {
                        return Err(Error::new("QuotaExceededError", "Blob exceeds size limit"));
                    }
                    bytes.extend_from_slice(&next);
                }
                Some(make_blob(bytes, &web_string(&options.get("type"))))
            }
            "Web.Blob.text" => Some(Value::promise(Ok(Value::string(String::from_utf8_lossy(
                &blob_bytes(&this),
            ))))),
            "Web.Blob.arrayBuffer" => {
                Some(Value::promise(Ok(make_array_buffer(&blob_bytes(&this)))))
            }
            "Web.Blob.slice" => {
                let bytes = blob_bytes(&this);
                let start = clamp_index(arg(&args, 0).number_value(), bytes.len());
                let end = if args.get(1).is_some() {
                    clamp_index(arg(&args, 1).number_value(), bytes.len())
                } else {
                    bytes.len()
                };
                let mime = if args.len() > 2 {
                    web_string(&args[2])
                } else {
                    this.get("type").text()
                };
                Some(make_blob(bytes[start.min(end)..end].to_vec(), &mime))
            }
            "Web.TextEncoder" if construct => {
                let value = web_object("TextEncoder");
                value.put("encoding", Value::string("utf-8"));
                value.put_hidden("__fatal", Value::Bool(false));
                Some(value)
            }
            "Web.TextEncoder.encode" => Some(make_uint8(web_string(&arg(&args, 0)).as_bytes())),
            "Web.TextDecoder" if construct => {
                let label = args
                    .first()
                    .map(web_string)
                    .unwrap_or_else(|| "utf-8".to_string())
                    .to_ascii_lowercase();
                if !matches!(label.as_str(), "utf-8" | "utf8" | "unicode-1-1-utf-8") {
                    return Err(Error::new("RangeError", "Only UTF-8 decoding is supported"));
                }
                let options = arg(&args, 1);
                let decoder = web_object("TextDecoder");
                decoder.put("encoding", Value::string("utf-8"));
                decoder.put_hidden("__fatal", Value::Bool(options.get("fatal").truthy()));
                decoder.put_hidden(
                    "__ignore_bom",
                    Value::Bool(options.get("ignoreBOM").truthy()),
                );
                Some(decoder)
            }
            "Web.TextDecoder.decode" => {
                let bytes = if args.is_empty() || matches!(arg(&args, 0), Value::Undefined) {
                    Vec::new()
                } else {
                    bytes_from_value(&args[0])
                };
                let text = utf8_decode(&bytes, this.get("__fatal").truthy())?;
                Some(Value::string(text))
            }
            "Web.btoa" => {
                let text = web_string(&arg(&args, 0));
                if !text.chars().all(|ch| (ch as u32) <= 0xff) {
                    return Err(Error::new(
                        "InvalidCharacterError",
                        "Input contains characters outside Latin-1",
                    ));
                }
                Some(Value::string(base64_encode(
                    &text.chars().map(|ch| ch as u8).collect::<Vec<_>>(),
                )))
            }
            "Web.atob" => Some(Value::string(String::from_utf8_lossy(&base64_decode(
                &web_string(&arg(&args, 0)),
            )?))),
            "Web.escape" => Some(Value::string(annex_b_escape(&web_string(&arg(&args, 0))))),
            "Web.unescape" => Some(Value::string(annex_b_unescape(&web_string(&arg(&args, 0))))),
            "Web.encodeURI" => Some(Value::string(encode_uri_component(
                &web_string(&arg(&args, 0)),
                false,
            ))),
            "Web.encodeURIComponent" => Some(Value::string(encode_uri_component(
                &web_string(&arg(&args, 0)),
                true,
            ))),
            "Web.decodeURI" => Some(Value::string(decode_uri_component(
                &web_string(&arg(&args, 0)),
                false,
            )?)),
            "Web.decodeURIComponent" => Some(Value::string(decode_uri_component(
                &web_string(&arg(&args, 0)),
                true,
            )?)),
            "Web.AbortController" if construct => {
                let signal = web_object("AbortSignal");
                signal.put("aborted", Value::Bool(false));
                signal.put("reason", Value::Undefined);
                signal.put_hidden("__abort_listeners", Value::array(Vec::new()));
                let controller = web_object("AbortController");
                controller.put("signal", signal);
                Some(controller)
            }
            "Web.AbortController.abort" => {
                let signal = this.get("signal");
                if !signal.get("aborted").truthy() {
                    let reason = args.first().cloned().unwrap_or_else(|| {
                        let exception = Value::object("DOMException");
                        if let Some(object) = exception.as_object() {
                            object.borrow_mut().prototype =
                                Some(self.global.get("DOMException").get("prototype"));
                        }
                        exception.put("name", Value::string("AbortError"));
                        exception.put("message", Value::string("The operation was aborted"));
                        exception
                    });
                    signal.put("aborted", Value::Bool(true));
                    signal.put("reason", reason.clone());
                    let event = web_object("Event");
                    event.put("type", Value::string("abort"));
                    event.put("bubbles", Value::Bool(false));
                    event.put("cancelable", Value::Bool(false));
                    self.dispatch_event(doc, signal.clone(), event)?;
                    for listener in signal.get("__abort_listeners").items() {
                        self.queue(listener, signal.clone(), vec![reason.clone()]);
                    }
                }
                Some(Value::Undefined)
            }
            "Web.AbortSignal.throwIfAborted" => {
                if this.get("aborted").truthy() {
                    return Err(Error::thrown(this.get("reason")));
                }
                Some(Value::Undefined)
            }
            "Web.MessageChannel" if construct => {
                let left_queue = Rc::new(RefCell::new(VecDeque::new()));
                let right_queue = Rc::new(RefCell::new(VecDeque::new()));
                let port1 = web_object("MessagePort");
                let port2 = web_object("MessagePort");
                if let Some(object) = port1.as_object() {
                    object.borrow_mut().payload = Some(Rc::new(Rc::new(RefCell::new(PortState {
                        inbox: left_queue.clone(),
                        peer_queue: right_queue.clone(),
                        peer: None,
                        started: true,
                        closed: false,
                    }))));
                }
                if let Some(object) = port2.as_object() {
                    object.borrow_mut().payload = Some(Rc::new(Rc::new(RefCell::new(PortState {
                        inbox: right_queue.clone(),
                        peer_queue: left_queue.clone(),
                        peer: None,
                        started: true,
                        closed: false,
                    }))));
                }
                if let Some(state) = port_payload(&port1) {
                    state.borrow_mut().peer = Some(port2.clone());
                }
                if let Some(state) = port_payload(&port2) {
                    state.borrow_mut().peer = Some(port1.clone());
                }
                let channel = web_object("MessageChannel");
                channel.put("port1", port1);
                channel.put("port2", port2);
                let mut ports = self.global.get("__web_ports").items();
                ports.push(channel.get("port1"));
                ports.push(channel.get("port2"));
                self.global.put_hidden("__web_ports", Value::array(ports));
                Some(channel)
            }
            "Web.MessagePort.postMessage" => {
                if let Some(port) = port_payload(&this) {
                    let peer = {
                        let port = port.borrow();
                        if port.closed {
                            return Ok(Some(Value::Undefined));
                        }
                        port.peer_queue.borrow_mut().push_back(arg(&args, 0));
                        port.peer.clone()
                    };
                    if let Some(peer) = peer {
                        self.drain_message_port(&peer);
                    }
                }
                Some(Value::Undefined)
            }
            "Web.MessagePort.start" => {
                if let Some(port) = port_payload(&this) {
                    port.borrow_mut().started = true;
                }
                self.drain_message_port(&this);
                Some(Value::Undefined)
            }
            "Web.MessagePort.close" => {
                if let Some(port) = port_payload(&this) {
                    port.borrow_mut().closed = true;
                }
                Some(Value::Undefined)
            }
            "Web.CustomElements.define" => {
                let registry = self.global.get("customElements");
                let tag = web_string(&arg(&args, 0)).to_ascii_lowercase();
                if tag.is_empty() || !tag.contains('-') {
                    return Err(Error::new(
                        "SyntaxError",
                        "Custom element names must contain a hyphen",
                    ));
                }
                if !matches!(registry.get(&tag), Value::Undefined) {
                    return Err(Error::new(
                        "NotSupportedError",
                        "Custom element name is already defined",
                    ));
                }
                registry.put(&tag, arg(&args, 1));
                let pending = self.global.get("__web_custom_pending").get(&tag).items();
                for promise in pending {
                    self.settle_promise(&promise, Ok(arg(&args, 1)));
                }
                self.global
                    .get("__web_custom_pending")
                    .put(&tag, Value::array(Vec::new()));
                Some(Value::Undefined)
            }
            "Web.CustomElements.get" => Some(
                self.global
                    .get("customElements")
                    .get(&web_string(&arg(&args, 0)).to_ascii_lowercase()),
            ),
            "Web.CustomElements.whenDefined" => {
                let tag = web_string(&arg(&args, 0)).to_ascii_lowercase();
                let definition = self.global.get("customElements").get(&tag);
                if !matches!(definition, Value::Undefined) {
                    Some(Value::promise(Ok(definition)))
                } else {
                    let promise = Value::pending_promise();
                    let pending_obj = self.global.get("__web_custom_pending");
                    let existing = pending_obj.get(&tag).items();
                    let mut next = existing;
                    next.push(promise.clone());
                    pending_obj.put(&tag, Value::array(next));
                    Some(promise)
                }
            }
            "Web.CustomElements.upgrade" => Some(Value::Undefined),
            "Web.URL" if construct => {
                let input = web_string(&arg(&args, 0));
                let base = args.get(1).map(web_string);
                let parsed = match base {
                    Some(base) => url::Url::options()
                        .base_url(url::Url::parse(&base).ok().as_ref())
                        .parse(&input),
                    None => url::Url::parse(&input),
                }
                .map_err(|error| Error::new("TypeError", error.to_string()))?;
                let result = web_object("URL");
                result.put("href", Value::string(parsed.as_str()));
                result.put("protocol", Value::string(format!("{}:", parsed.scheme())));
                result.put("host", Value::string(parsed.host_str().unwrap_or("")));
                result.put("hostname", Value::string(parsed.host_str().unwrap_or("")));
                result.put("pathname", Value::string(parsed.path()));
                result.put(
                    "search",
                    Value::string(parsed.query().map(|v| format!("?{v}")).unwrap_or_default()),
                );
                result.put(
                    "hash",
                    Value::string(
                        parsed
                            .fragment()
                            .map(|v| format!("#{v}"))
                            .unwrap_or_default(),
                    ),
                );
                result.put(
                    "origin",
                    Value::string(parsed.origin().ascii_serialization()),
                );
                Some(result)
            }
            "Web.URL.createObjectURL" => {
                if arg(&args, 0).kind() != "Blob" {
                    return Err(Error::type_error("createObjectURL expects a Blob"));
                }
                let id = self.global.get("__web_url_next").number_value() as u64;
                self.global
                    .put_hidden("__web_url_next", Value::Number(id.saturating_add(1) as f64));
                let blob = arg(&args, 0);
                let mime = blob.get("type").text();
                let media_type = if mime.is_empty() {
                    "application/octet-stream"
                } else {
                    &mime
                };
                let url = format!(
                    "data:{media_type};base64,{}",
                    base64_encode(&blob_bytes(&blob))
                );
                let registry = self.global.get("__web_object_urls");
                registry.put(&url, blob);
                Some(Value::string(url))
            }
            "Web.URL.revokeObjectURL" => {
                self.global
                    .get("__web_object_urls")
                    .put(&web_string(&arg(&args, 0)), Value::Undefined);
                Some(Value::Undefined)
            }
            "Web.Intl.NumberFormat" if construct => {
                let locale = args
                    .first()
                    .map(web_string)
                    .unwrap_or_else(|| "en-US".to_string());
                if !["en-US", "de-DE"].iter().any(|supported| {
                    locale == *supported || locale.starts_with(&format!("{supported}-"))
                }) {
                    return Err(Error::new(
                        "RangeError",
                        "number formatting locale is not admitted",
                    ));
                }
                let options = arg(&args, 1);
                if !matches!(
                    web_string(&options.get("style")).as_str(),
                    "undefined" | "decimal"
                ) {
                    return Err(Error::new(
                        "RangeError",
                        "Only decimal Intl.NumberFormat style is available",
                    ));
                }
                let minimum = options.get("minimumFractionDigits").number_value();
                let maximum = options.get("maximumFractionDigits").number_value();
                if (minimum.is_finite() && !(0.0..=20.0).contains(&minimum))
                    || (maximum.is_finite() && !(0.0..=20.0).contains(&maximum))
                    || (minimum.is_finite() && maximum.is_finite() && minimum > maximum)
                {
                    return Err(Error::new(
                        "RangeError",
                        "fraction digit options must be ordered values from 0 through 20",
                    ));
                }
                let formatter = web_object("NumberFormat");
                formatter.put(
                    "locale",
                    Value::string(if locale.starts_with("de-DE") {
                        "de-DE"
                    } else {
                        "en-US"
                    }),
                );
                formatter.put_hidden("__options", options);
                formatter.put("format", web_fn("Intl.NumberFormat.format"));
                Some(formatter)
            }
            "Web.Intl.NumberFormat.format" => {
                let text = locale_number(arg(&args, 0).number_value(), &this.get("__options"));
                Some(Value::string(if this.get("locale").text() == "de-DE" {
                    text.chars()
                        .map(|ch| match ch {
                            ',' => '.',
                            '.' => ',',
                            _ => ch,
                        })
                        .collect::<String>()
                } else {
                    text
                }))
            }
            "Web.Intl.NumberFormat.resolvedOptions" => {
                let options = web_object("Object");
                options.put("locale", this.get("locale"));
                options.put("numberingSystem", Value::string("latn"));
                options.put("style", Value::string("decimal"));
                let formatter_options = this.get("__options");
                options.put(
                    "useGrouping",
                    Value::Bool(!matches!(
                        formatter_options.get("useGrouping"),
                        Value::Bool(false)
                    )),
                );
                let minimum = formatter_options
                    .get("minimumFractionDigits")
                    .number_value();
                let maximum = formatter_options
                    .get("maximumFractionDigits")
                    .number_value();
                options.put(
                    "minimumFractionDigits",
                    Value::Number(if minimum.is_finite() { minimum } else { 0.0 }),
                );
                options.put(
                    "maximumFractionDigits",
                    Value::Number(if maximum.is_finite() { maximum } else { 3.0 }),
                );
                Some(options)
            }
            _ => None,
        };
        let _ = doc;
        Ok(value)
    }

    fn settle_promise(&mut self, promise: &Value, result: Result<Value>) {
        if promise.kind() != "Promise" || promise.get("state").number_value() != 0.0 {
            return;
        }
        match result {
            Ok(value) => {
                promise.put_hidden("state", Value::Number(1.0));
                promise.put_hidden("value", value);
            }
            Err(error) => {
                let thrown = error
                    .thrown
                    .unwrap_or_else(|| Value::error(error.name, error.message));
                promise.put_hidden("state", Value::Number(2.0));
                promise.put_hidden("value", thrown);
            }
        }
        self.schedule_promise_reactions(promise);
    }

    fn next_web_id(&mut self) -> u64 {
        let id = self
            .global
            .get("__web_promise_callback_next")
            .number_value() as u64;
        self.global.put(
            "__web_promise_callback_next",
            Value::Number(id.saturating_add(1) as f64),
        );
        id
    }

    pub(super) fn resolve_promise(
        &mut self,
        doc: &mut blitz_dom::BaseDocument,
        promise: &Value,
        value: Value,
        rejected: bool,
    ) -> Result<()> {
        if promise.kind() != "Promise" || promise.get("state").number_value() != 0.0 {
            return Ok(());
        }
        if rejected {
            self.settle_promise(promise, Err(Error::thrown(value)));
            return Ok(());
        }
        if value.same(promise) {
            self.settle_promise(
                promise,
                Err(Error::type_error("promise cannot resolve to itself")),
            );
            return Ok(());
        }
        if value.kind() == "Promise" {
            match value.get("state").number_value() as u8 {
                1 => return self.resolve_promise(doc, promise, value.get("value"), false),
                2 => return self.resolve_promise(doc, promise, value.get("value"), true),
                _ => {}
            }
            let id = self.next_web_id();
            self.global
                .get("__web_promise_callbacks")
                .put(&id.to_string(), promise.clone());
            let reaction = web_object("PromiseReaction");
            reaction.put_hidden("fulfilled", web_fn(&format!("PromiseAdoptResolve.{id}")));
            reaction.put_hidden("rejected", web_fn(&format!("PromiseAdoptReject.{id}")));
            reaction.put_hidden("next", Value::pending_promise());
            value
                .as_object()
                .ok_or_else(|| Error::type_error("invalid promise value"))?
                .borrow_mut()
                .indexed
                .push(reaction);
            self.schedule_promise_reactions(&value);
            return Ok(());
        }
        if value.as_object().is_some() {
            let then = self.get_property(doc, &value, "then")?;
            if matches!(then.kind().as_str(), "Function" | "Native") {
                let id = self.next_web_id();
                self.global
                    .get("__web_promise_callbacks")
                    .put(&id.to_string(), promise.clone());
                let resolve = web_fn(&format!("PromiseAdoptResolve.{id}"));
                let reject = web_fn(&format!("PromiseAdoptReject.{id}"));
                if let Err(error) = self.call(doc, then, value, vec![resolve, reject], false) {
                    self.resolve_promise(doc, promise, error.value(), true)?;
                }
                return Ok(());
            }
        }
        self.settle_promise(promise, Ok(value));
        Ok(())
    }

    fn attach_all_item(
        &mut self,
        doc: &mut blitz_dom::BaseDocument,
        id: u64,
        index: usize,
        value: Value,
    ) -> Result<()> {
        let fulfilled = web_fn(&format!("PromiseAllItem.{id}.{index}"));
        let rejected = web_fn(&format!("PromiseAllReject.{id}"));
        let adopted = if value.kind() == "Promise" {
            Some(value.clone())
        } else if value.as_object().is_some() {
            let promise = Value::pending_promise();
            self.resolve_promise(doc, &promise, value.clone(), false)?;
            Some(promise)
        } else {
            None
        };
        if let Some(value) = adopted {
            let reaction = web_object("PromiseReaction");
            reaction.put_hidden("fulfilled", fulfilled);
            reaction.put_hidden("rejected", rejected);
            reaction.put_hidden("next", Value::pending_promise());
            value
                .as_object()
                .ok_or_else(|| Error::type_error("invalid promise value"))?
                .borrow_mut()
                .indexed
                .push(reaction);
            self.schedule_promise_reactions(&value);
        } else {
            self.call(doc, fulfilled, Value::Undefined, vec![value], false)?;
        }
        Ok(())
    }

    fn attach_race_item(
        &mut self,
        doc: &mut blitz_dom::BaseDocument,
        id: u64,
        value: Value,
    ) -> Result<()> {
        let fulfilled = web_fn(&format!("PromiseRace.{id}"));
        let rejected = web_fn(&format!("PromiseRaceReject.{id}"));
        let adopted = if value.kind() == "Promise" {
            Some(value.clone())
        } else if value.as_object().is_some() {
            let promise = Value::pending_promise();
            self.resolve_promise(doc, &promise, value.clone(), false)?;
            Some(promise)
        } else {
            None
        };
        if let Some(value) = adopted {
            let reaction = web_object("PromiseReaction");
            reaction.put_hidden("fulfilled", fulfilled);
            reaction.put_hidden("rejected", rejected);
            reaction.put_hidden("next", Value::pending_promise());
            value
                .as_object()
                .ok_or_else(|| Error::type_error("invalid promise value"))?
                .borrow_mut()
                .indexed
                .push(reaction);
            self.schedule_promise_reactions(&value);
        } else {
            let promise = self.global.get("__web_aggregates").get(&id.to_string());
            self.resolve_promise(doc, &promise, value, false)?;
            self.global
                .get("__web_aggregates")
                .put(&id.to_string(), Value::Undefined);
        }
        Ok(())
    }

    fn schedule_promise_reactions(&mut self, promise: &Value) {
        let state = promise.get("state").number_value() as u8;
        if state == 0 {
            return;
        }
        let reactions = promise
            .as_object()
            .map(|object| object.borrow().indexed.clone())
            .unwrap_or_default();
        if let Some(object) = promise.as_object() {
            object.borrow_mut().indexed.clear();
        }
        for reaction in reactions {
            let callback = web_fn("PromiseReaction");
            self.queue(
                callback,
                Value::Undefined,
                vec![
                    promise.clone(),
                    reaction,
                    Value::Bool(state == 2),
                    promise.get("value"),
                ],
            );
        }
    }

    fn drain_message_port(&mut self, port: &Value) {
        let Some(state) = port_payload(port) else {
            return;
        };
        if !state.borrow().started || state.borrow().closed {
            return;
        }
        let callback = port.get("onmessage");
        if !matches!(callback.kind().as_str(), "Function" | "Native") {
            return;
        }
        let mut messages = Vec::new();
        while let Some(message) = state.borrow().inbox.borrow_mut().pop_front() {
            messages.push(message);
        }
        for data in messages {
            let event = web_object("MessageEvent");
            event.put("data", data);
            event.put("target", port.clone());
            event.put("currentTarget", port.clone());
            self.queue(callback.clone(), port.clone(), vec![event]);
        }
    }

    pub(super) fn web_poll(&mut self, doc: &mut blitz_dom::BaseDocument) -> Result<()> {
        for port in self.global.get("__web_ports").items() {
            self.drain_message_port(&port);
        }
        for observer in self.global.get("__web_observers").items() {
            let kind = observer.kind();
            let mut records = Vec::new();
            for registration in observer.get("observed").items() {
                let target = registration.get("target");
                let Some(id) = observer_node_id(&target) else {
                    continue;
                };
                let Some(node) = doc.get_node(id) else {
                    continue;
                };
                if kind == "MutationObserver" {
                    let attributes = attributes_snapshot(node);
                    let text = node
                        .text_data()
                        .map_or("".to_owned(), |text| text.content.clone());
                    let children = Value::array(
                        node.children
                            .iter()
                            .map(|child| Value::Number(child.as_u64() as f64))
                            .collect(),
                    );
                    if registration.get("attributes").truthy() {
                        let old = registration.get("oldAttributes");
                        let mut names = Vec::new();
                        for source in [&old, &attributes] {
                            if let Some(object) = source.as_object() {
                                names.extend(object.borrow().props.keys().cloned());
                            }
                        }
                        names.sort();
                        names.dedup();
                        let filter = registration
                            .get("attributeFilter")
                            .items()
                            .into_iter()
                            .map(|value| value.text())
                            .collect::<Vec<_>>();
                        for name in names {
                            let old_value = old.get(&name);
                            let new_value = attributes.get(&name);
                            if old_value.same(&new_value) {
                                continue;
                            }
                            if !filter.is_empty() && !filter.iter().any(|field| field == &name) {
                                continue;
                            }
                            let record = mutation_record("attributes", target.clone());
                            record.put("attributeName", Value::string(&name));
                            if registration.get("attributeOldValue").truthy() {
                                record.put("oldValue", old_value);
                            }
                            records.push(record);
                        }
                    }
                    if registration.get("characterData").truthy()
                        && text != registration.get("oldText").text()
                    {
                        let record = mutation_record("characterData", target.clone());
                        if registration.get("characterDataOldValue").truthy() {
                            record.put("oldValue", registration.get("oldText"));
                        }
                        records.push(record);
                    }
                    let old_children = registration.get("oldChildren").items();
                    let current_children = children.items();
                    let children_changed = old_children.len() != current_children.len()
                        || old_children
                            .iter()
                            .zip(&current_children)
                            .any(|(old, current)| old.number_value() != current.number_value());
                    if registration.get("childList").truthy() && children_changed {
                        let added = current_children
                            .iter()
                            .filter(|child| {
                                !old_children
                                    .iter()
                                    .any(|previous| previous.number_value() == child.number_value())
                            })
                            .map(|child| self.node(NodeId::from_u64(child.number_value() as u64)))
                            .collect();
                        let removed = old_children
                            .iter()
                            .filter(|child| {
                                !current_children
                                    .iter()
                                    .any(|current| current.number_value() == child.number_value())
                            })
                            .map(|child| self.node(NodeId::from_u64(child.number_value() as u64)))
                            .collect();
                        let record = mutation_record("childList", target.clone());
                        record.put("addedNodes", Value::array(added));
                        record.put("removedNodes", Value::array(removed));
                        records.push(record);
                    }
                    registration.put("oldAttributes", attributes);
                    registration.put("oldText", Value::string(text));
                    registration.put("oldChildren", children);
                } else {
                    let rect = if kind == "ResizeObserver" {
                        content_rect(node)
                    } else {
                        node_rect(node)
                    };
                    let previous = registration.get("oldRect").items();
                    let current = rect.items();
                    if previous.len() != current.len()
                        || previous
                            .iter()
                            .zip(&current)
                            .any(|(a, b)| a.number_value() != b.number_value())
                    {
                        let entry = if kind == "ResizeObserver" {
                            let entry = web_object("ResizeObserverEntry");
                            entry.put("target", target.clone());
                            entry.put("contentRect", rect.clone());
                            entry.put(
                                "borderBoxSize",
                                Value::array(vec![size_from_rect(&node_rect(node))]),
                            );
                            entry.put("contentBoxSize", Value::array(vec![size_from_rect(&rect)]));
                            let device_size = size_from_rect(&rect);
                            let scale = doc.get_viewport().scale_f64();
                            device_size.put(
                                "inlineSize",
                                Value::Number(rect.get("width").number_value() * scale),
                            );
                            device_size.put(
                                "blockSize",
                                Value::Number(rect.get("height").number_value() * scale),
                            );
                            entry.put("devicePixelContentBoxSize", Value::array(vec![device_size]));
                            entry
                        } else {
                            intersection_entry(doc, target.clone(), &rect)
                        };
                        records.push(entry);
                        registration.put("oldRect", rect);
                    }
                }
            }
            if !records.is_empty() {
                let callback = observer.get("callback");
                self.queue(
                    callback,
                    Value::Undefined,
                    vec![Value::array(records), observer.clone()],
                );
            }
        }
        Ok(())
    }
}

fn observer_node_id(target: &Value) -> Option<NodeId> {
    if let Some(id) = target.node_id() {
        return Some(id);
    }
    let id = target.get("__node").number_value();
    (id.is_finite() && id >= 0.0).then_some(NodeId::from_u64(id as u64))
}

fn node_rect(node: &blitz_dom::Node) -> Value {
    let layout = node.final_layout();
    let rect = web_object("DOMRectReadOnly");
    rect.put("x", Value::Number(layout.location.x as f64));
    rect.put("y", Value::Number(layout.location.y as f64));
    rect.put("width", Value::Number(layout.size.width as f64));
    rect.put("height", Value::Number(layout.size.height as f64));
    rect.put("top", Value::Number(layout.location.y as f64));
    rect.put("left", Value::Number(layout.location.x as f64));
    rect.put(
        "right",
        Value::Number((layout.location.x + layout.size.width) as f64),
    );
    rect.put(
        "bottom",
        Value::Number((layout.location.y + layout.size.height) as f64),
    );
    rect
}

fn content_rect(node: &blitz_dom::Node) -> Value {
    let layout = node.final_layout();
    let x = layout.location.x + layout.content_box_x();
    let y = layout.location.y + layout.content_box_y();
    let width = layout.content_box_width();
    let height = layout.content_box_height();
    let rect = web_object("DOMRectReadOnly");
    rect.put("x", Value::Number(x as f64));
    rect.put("y", Value::Number(y as f64));
    rect.put("width", Value::Number(width as f64));
    rect.put("height", Value::Number(height as f64));
    rect.put("top", Value::Number(y as f64));
    rect.put("left", Value::Number(x as f64));
    rect.put("right", Value::Number((x + width) as f64));
    rect.put("bottom", Value::Number((y + height) as f64));
    rect
}

fn size_from_rect(rect: &Value) -> Value {
    let size = web_object("ResizeObserverSize");
    size.put("inlineSize", rect.get("width"));
    size.put("blockSize", rect.get("height"));
    size
}

fn mutation_record(kind: &str, target: Value) -> Value {
    let record = web_object("MutationRecord");
    record.put("type", Value::string(kind));
    record.put("target", target);
    record.put("addedNodes", Value::array(Vec::new()));
    record.put("removedNodes", Value::array(Vec::new()));
    record.put("attributeName", Value::Null);
    record.put("attributeNamespace", Value::Null);
    record.put("oldValue", Value::Null);
    record.put("previousSibling", Value::Null);
    record.put("nextSibling", Value::Null);
    record
}

fn attributes_snapshot(node: &blitz_dom::Node) -> Value {
    let snapshot = web_object("Object");
    for attribute in node.attrs().unwrap_or(&[]) {
        snapshot.put(
            attribute.name.local.as_ref(),
            Value::string(&attribute.value),
        );
    }
    snapshot
}

fn intersection_entry(doc: &blitz_dom::BaseDocument, target: Value, rect: &Value) -> Value {
    let viewport = doc.get_viewport();
    let scale = viewport.scale_f64();
    let viewport_width = viewport.window_size.0 as f64 / scale;
    let viewport_height = viewport.window_size.1 as f64 / scale;
    let right = viewport_width.min(rect.get("right").number_value());
    let bottom = viewport_height.min(rect.get("bottom").number_value());
    let left = 0.0_f64.max(rect.get("left").number_value());
    let top = 0.0_f64.max(rect.get("top").number_value());
    let width = (right - left).max(0.0);
    let height = (bottom - top).max(0.0);
    let area = rect.get("width").number_value() * rect.get("height").number_value();
    let intersection = web_object("DOMRectReadOnly");
    for (key, value) in [
        ("x", left),
        ("y", top),
        ("width", width),
        ("height", height),
        ("top", top),
        ("left", left),
        ("right", right.max(left)),
        ("bottom", bottom.max(top)),
    ] {
        intersection.put(key, Value::Number(value));
    }
    let entry = web_object("IntersectionObserverEntry");
    entry.put("target", target);
    entry.put("boundingClientRect", rect.clone());
    entry.put("intersectionRect", intersection);
    let root_bounds = web_object("DOMRectReadOnly");
    for (key, value) in [
        ("x", 0.0),
        ("y", 0.0),
        ("left", 0.0),
        ("top", 0.0),
        ("right", viewport_width),
        ("bottom", viewport_height),
        ("width", viewport_width),
        ("height", viewport_height),
    ] {
        root_bounds.put(key, Value::Number(value));
    }
    entry.put("rootBounds", root_bounds);
    entry.put("isIntersecting", Value::Bool(width > 0.0 && height > 0.0));
    entry.put(
        "intersectionRatio",
        Value::Number(if area <= 0.0 {
            0.0
        } else {
            width * height / area
        }),
    );
    entry
}

fn port_payload(port: &Value) -> Option<Rc<RefCell<PortState>>> {
    port.as_object().and_then(|object| {
        object
            .borrow()
            .payload
            .as_ref()
            .and_then(|payload| payload.downcast_ref::<Rc<RefCell<PortState>>>().cloned())
    })
}

fn clamp_index(value: f64, length: usize) -> usize {
    if !value.is_finite() {
        return 0;
    }
    let index = value.trunc() as isize;
    if index < 0 {
        length.saturating_sub(index.unsigned_abs())
    } else {
        (index as usize).min(length)
    }
}

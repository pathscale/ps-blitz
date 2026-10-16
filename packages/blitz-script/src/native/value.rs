use std::any::Any;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::rc::Rc;

use super::syntax::{Pattern, Stmt};
use blitz_dom::NodeId;

pub type Result<T, E = Error> = std::result::Result<T, E>;
pub type Env = Rc<RefCell<Environment>>;

#[derive(Clone)]
pub struct Error {
    pub name: String,
    pub message: String,
    pub thrown: Option<Value>,
    pub stack: Vec<String>,
}
impl Error {
    pub fn new(name: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            message: message.into(),
            thrown: None,
            stack: Vec::new(),
        }
    }
    pub fn type_error(message: impl Into<String>) -> Self {
        Self::new("TypeError", message)
    }
    pub fn syntax(message: impl Into<String>) -> Self {
        Self::new("SyntaxError", message)
    }
    pub fn thrown(value: Value) -> Self {
        let stack = value
            .get("\0error_stack")
            .items()
            .iter()
            .take(64)
            .map(Value::text)
            .collect();
        Self {
            name: value.get("name").text(),
            message: value.get("message").text(),
            thrown: Some(value),
            stack,
        }
    }
    pub fn value(&self) -> Value {
        let value = self
            .thrown
            .clone()
            .unwrap_or_else(|| Value::error(&self.name, &self.message));
        value.put_hidden(
            "\0error_stack",
            Value::array(self.stack.iter().map(Value::string).collect()),
        );
        value
    }
}
impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.name, self.message)?;
        for frame in &self.stack {
            write!(f, "\n  at {frame}")?;
        }
        let mut seen = Vec::new();
        let mut cause = self.thrown.as_ref().map(|value| value.get("cause"));
        while let Some(value) = cause {
            if matches!(value, Value::Undefined)
                || seen.len() >= 16
                || seen.iter().any(|prior: &Value| prior.same(&value))
            {
                break;
            }
            write!(
                f,
                "\nCaused by: {}: {}",
                value.get("name").text(),
                value.get("message").text()
            )?;
            for frame in value.get("\0error_stack").items().iter().take(64) {
                write!(f, "\n  at {}", frame.text())?;
            }
            cause = Some(value.get("cause"));
            seen.push(value);
        }
        Ok(())
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}
impl std::error::Error for Error {}

#[derive(Clone, Default)]
pub enum Value {
    #[default]
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    String(Rc<[u16]>),
    Object(Rc<RefCell<Object>>),
}
impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Object(object) => write!(
                f,
                "[{}]",
                object
                    .try_borrow()
                    .map_or_else(|_| String::from("borrowed"), |o| o.kind.clone())
            ),
            _ => write!(f, "{}", self.text()),
        }
    }
}
#[derive(Clone, Debug)]
pub struct Property {
    pub value: Value,
    pub get: Option<Value>,
    pub set: Option<Value>,
    pub enumerable: bool,
}
impl Property {
    pub fn data(value: Value) -> Self {
        Self {
            value,
            get: None,
            set: None,
            enumerable: true,
        }
    }
}
pub struct Object {
    pub kind: String,
    pub native: Option<String>,
    pub realm: Option<u64>,
    pub props: BTreeMap<String, Property>,
    pub indexed: Vec<Value>,
    pub prototype: Option<Value>,
    pub function: Option<Rc<Function>>,
    pub payload: Option<Rc<dyn Any>>,
    pub frozen: bool,
}
impl Object {
    pub fn new(kind: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            native: None,
            realm: None,
            props: BTreeMap::new(),
            indexed: Vec::new(),
            prototype: None,
            function: None,
            payload: None,
            frozen: false,
        }
    }
}
#[derive(Clone)]
pub struct Function {
    pub params: Vec<Pattern>,
    pub body: Vec<Stmt>,
    pub env: Env,
    pub arrow: bool,
    pub asynchronous: bool,
    pub source_offset: Option<usize>,
    pub this: Value,
    pub name: Option<String>,
}
pub struct Environment {
    pub bindings: BTreeMap<String, Value>,
    pub parent: Option<Env>,
}
impl Environment {
    pub fn new(parent: Option<Env>) -> Env {
        let environment = Rc::new(RefCell::new(Self {
            bindings: BTreeMap::new(),
            parent,
        }));
        track_environment(&environment);
        environment
    }
}
impl Value {
    pub fn string(text: impl AsRef<str>) -> Self {
        Self::String(text.as_ref().encode_utf16().collect::<Vec<_>>().into())
    }
    pub fn object(kind: impl Into<String>) -> Self {
        let object = Rc::new(RefCell::new(Object::new(kind)));
        track_object(&object);
        Self::Object(object)
    }
    pub fn number(number: impl Into<f64>) -> Self {
        Self::Number(number.into())
    }
    pub fn array(items: Vec<Value>) -> Self {
        let value = Self::object("Array");
        if let Self::Object(object) = &value {
            object.borrow_mut().indexed = items;
        }
        value
    }
    pub fn native(name: impl Into<String>) -> Self {
        let value = Self::object("Native");
        if let Some(object) = value.as_object() {
            object.borrow_mut().native = Some(name.into());
        }
        value
    }
    pub fn promise(result: Result<Value>) -> Self {
        let promise = Self::pending_promise();
        let (state, value) = match result {
            Ok(value) => (1.0, value),
            Err(error) => (2.0, error.value()),
        };
        promise.put_hidden("state", Self::Number(state));
        promise.put_hidden("value", value);
        promise
    }
    pub fn pending_promise() -> Self {
        let value = Self::object("Promise");
        value.put_hidden("state", Self::Number(0.0));
        value.put_hidden("value", Self::Undefined);
        value
    }
    pub fn error(name: impl AsRef<str>, message: impl AsRef<str>) -> Self {
        let value = Self::object("Error");
        value.put("name", Self::string(name));
        value.put("message", Self::string(message));
        value
    }
    pub fn as_object(&self) -> Option<Rc<RefCell<Object>>> {
        if let Self::Object(o) = self {
            Some(Rc::clone(o))
        } else {
            None
        }
    }
    pub fn kind(&self) -> String {
        match self {
            Self::Undefined => "Undefined".into(),
            Self::Null => "Null".into(),
            Self::Bool(_) => "Boolean".into(),
            Self::Number(_) => "Number".into(),
            Self::String(_) => "String".into(),
            Self::Object(o) => o.borrow().kind.clone(),
        }
    }
    pub fn get(&self, key: &str) -> Value {
        let Self::Object(object) = self else {
            return Self::Undefined;
        };
        let object = object.borrow();
        if key == "__native" {
            if let Some(name) = &object.native {
                return Self::string(name);
            }
        }
        if let Some(property) = object.props.get(key) {
            return property.value.clone();
        }
        if key == "length"
            && matches!(
                object.kind.as_str(),
                "Array"
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
        {
            return Self::Number(object.indexed.len() as f64);
        }
        if let Ok(index) = key.parse::<usize>() {
            return object.indexed.get(index).cloned().unwrap_or_default();
        }
        Self::Undefined
    }
    pub fn put(&self, key: &str, value: Value) {
        let Self::Object(object) = self else {
            return;
        };
        let mut object = object.borrow_mut();
        if object.frozen {
            return;
        }
        if let Ok(index) = key.parse::<usize>() {
            if index < 1_048_576
                && (index < object.indexed.len()
                    || matches!(
                        object.kind.as_str(),
                        "Array"
                            | "Uint8Array"
                            | "Uint8ClampedArray"
                            | "Uint16Array"
                            | "Uint32Array"
                            | "Int8Array"
                            | "Int16Array"
                            | "Int32Array"
                            | "Float32Array"
                            | "Float64Array"
                    ))
            {
                if index >= object.indexed.len() {
                    object.indexed.resize(index + 1, Self::Undefined);
                }
                object.indexed[index] = value;
                return;
            }
        }
        if key == "length" && object.kind == "Array" {
            let length = value.number_value();
            if length.is_finite() && (0.0..=1_048_576.0).contains(&length) {
                object.indexed.resize(length as usize, Self::Undefined);
            }
            return;
        }
        if let Some(property) = object.props.get_mut(key) {
            property.value = value;
        } else {
            object.props.insert(key.into(), Property::data(value));
        }
    }
    /// Store implementation-owned metadata without exposing it to enumeration.
    pub fn put_hidden(&self, key: &str, value: Value) {
        let Self::Object(object) = self else {
            return;
        };
        let mut object = object.borrow_mut();
        if object.frozen {
            return;
        }
        object.props.insert(
            key.into(),
            Property {
                value,
                get: None,
                set: None,
                enumerable: false,
            },
        );
    }
    pub fn items(&self) -> Vec<Value> {
        match self {
            Self::Object(o) => o.borrow().indexed.clone(),
            Self::String(s) => s
                .iter()
                .map(|unit| Self::String(Rc::from([*unit])))
                .collect(),
            _ => Vec::new(),
        }
    }
    pub fn node_id(&self) -> Option<NodeId> {
        let object = self.as_object()?;
        let object = object.borrow();
        if let Some(id) = object
            .payload
            .as_ref()
            .and_then(|payload| payload.downcast_ref::<NodeId>())
        {
            return Some(*id);
        }
        object
            .props
            .get("__node")?
            .value
            .text()
            .parse::<u64>()
            .ok()
            .map(NodeId::from_u64)
    }
    pub fn same(&self, other: &Value) -> bool {
        match (self, other) {
            (Self::Undefined, Self::Undefined) | (Self::Null, Self::Null) => true,
            (Self::Bool(a), Self::Bool(b)) => a == b,
            (Self::Number(a), Self::Number(b)) => a == b,
            (Self::String(a), Self::String(b)) => a == b,
            (Self::Object(a), Self::Object(b)) => Rc::ptr_eq(a, b),
            _ => false,
        }
    }
    pub fn truthy(&self) -> bool {
        match self {
            Self::Undefined | Self::Null => false,
            Self::Bool(v) => *v,
            Self::Number(v) => *v != 0.0 && !v.is_nan(),
            Self::String(v) => !v.is_empty(),
            Self::Object(_) => true,
        }
    }
    pub fn number_value(&self) -> f64 {
        match self {
            Self::Undefined => f64::NAN,
            Self::Null => 0.0,
            Self::Bool(v) => u8::from(*v) as f64,
            Self::Number(v) => *v,
            Self::String(s) => {
                let s = String::from_utf16_lossy(s);
                let s = s.trim();
                if s.is_empty() {
                    0.0
                } else if let Some(v) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
                    u64::from_str_radix(v, 16).map_or(f64::NAN, |v| v as f64)
                } else {
                    s.parse().unwrap_or(f64::NAN)
                }
            }
            Self::Object(_) => self.text().parse().unwrap_or(f64::NAN),
        }
    }
    pub fn text(&self) -> String {
        match self {
            Self::Undefined => "undefined".into(),
            Self::Null => "null".into(),
            Self::Bool(v) => v.to_string(),
            Self::Number(v) => {
                if v.is_nan() {
                    "NaN".into()
                } else if *v == 0.0 {
                    "0".into()
                } else {
                    v.to_string()
                }
            }
            Self::String(v) => String::from_utf16_lossy(v),
            Self::Object(o) => {
                let o = o.borrow();
                if matches!(
                    o.kind.as_str(),
                    "Array" | "Uint8Array" | "Uint8ClampedArray" | "Uint32Array" | "Float32Array"
                ) {
                    o.indexed
                        .iter()
                        .map(|v| {
                            if matches!(v, Self::Null | Self::Undefined) {
                                String::new()
                            } else {
                                v.text()
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(",")
                } else if o.kind == "Error" || o.kind == "DOMException" {
                    let name = o
                        .props
                        .get("name")
                        .map_or("Error".into(), |p| p.value.text());
                    let message = o
                        .props
                        .get("message")
                        .map_or(String::new(), |p| p.value.text());
                    if message.is_empty() {
                        name
                    } else {
                        format!("{name}: {message}")
                    }
                } else if o.kind == "URL" {
                    o.props
                        .get("href")
                        .map_or(String::new(), |p| p.value.text())
                } else {
                    format!(
                        "[object {}]",
                        if o.kind == "Object" {
                            "Object"
                        } else {
                            &o.kind
                        }
                    )
                }
            }
        }
    }
    pub fn from_json(value: serde_json::Value) -> Value {
        match value {
            serde_json::Value::Null => Self::Null,
            serde_json::Value::Bool(v) => Self::Bool(v),
            serde_json::Value::Number(v) => Self::Number(v.as_f64().unwrap_or(f64::NAN)),
            serde_json::Value::String(v) => Self::string(v),
            serde_json::Value::Array(v) => {
                Self::array(v.into_iter().map(Self::from_json).collect())
            }
            serde_json::Value::Object(v) => {
                let object = Self::object("Object");
                for (k, v) in v {
                    object.put(&k, Self::from_json(v));
                }
                object
            }
        }
    }
    pub fn to_json(&self) -> Result<serde_json::Value> {
        self.json_inner(&mut BTreeSet::new(), 0)
    }
    fn json_inner(&self, seen: &mut BTreeSet<usize>, depth: usize) -> Result<serde_json::Value> {
        if depth > 128 {
            return Err(Error::new("RangeError", "JSON nesting limit exceeded"));
        }
        Ok(match self {
            Self::Undefined | Self::Null => serde_json::Value::Null,
            Self::Bool(v) => serde_json::Value::Bool(*v),
            Self::Number(v)
                if v.is_finite()
                    && v.fract() == 0.0
                    && *v >= i64::MIN as f64
                    && *v < i64::MAX as f64 =>
            {
                serde_json::Value::Number(serde_json::Number::from(*v as i64))
            }
            Self::Number(v)
                if v.is_finite() && v.fract() == 0.0 && *v >= 0.0 && *v < u64::MAX as f64 =>
            {
                serde_json::Value::Number(serde_json::Number::from(*v as u64))
            }
            Self::Number(v) => serde_json::Number::from_f64(*v)
                .map_or(serde_json::Value::Null, serde_json::Value::Number),
            Self::String(v) => serde_json::Value::String(String::from_utf16_lossy(v)),
            Self::Object(o) => {
                let id = Rc::as_ptr(o) as usize;
                if !seen.insert(id) {
                    return Err(Error::type_error(
                        "cyclic value cannot be represented as JSON",
                    ));
                }
                let (kind, items, properties) = {
                    let o = o.borrow();
                    (o.kind.clone(), o.indexed.clone(), o.props.clone())
                };
                let result = if matches!(kind.as_str(), "Array" | "NodeList" | "HTMLCollection") {
                    serde_json::Value::Array(
                        items
                            .iter()
                            .map(|v| v.json_inner(seen, depth + 1))
                            .collect::<Result<_>>()?,
                    )
                } else {
                    let mut map = serde_json::Map::new();
                    for (k, p) in properties {
                        if p.enumerable
                            && !matches!(p.value, Self::Undefined)
                            && !matches!(p.value.kind().as_str(), "Native" | "Function")
                        {
                            map.insert(k, p.value.json_inner(seen, depth + 1)?);
                        }
                    }
                    serde_json::Value::Object(map)
                };
                seen.remove(&id);
                result
            }
        })
    }
}

#[derive(Default)]
pub(super) struct RealmHeap {
    pub(super) realm: u64,
    objects: Vec<std::rc::Weak<RefCell<Object>>>,
    environments: Vec<std::rc::Weak<RefCell<Environment>>>,
    exhausted: bool,
}
thread_local! {
    static ACTIVE_HEAP: RefCell<Option<std::rc::Weak<RefCell<RealmHeap>>>> = const { RefCell::new(None) };
}
pub(super) struct HeapScope(Option<std::rc::Weak<RefCell<RealmHeap>>>);
impl HeapScope {
    pub(super) fn enter(heap: &Rc<RefCell<RealmHeap>>) -> Self {
        Self(ACTIVE_HEAP.with(|active| active.replace(Some(Rc::downgrade(heap)))))
    }
}
impl Drop for HeapScope {
    fn drop(&mut self) {
        ACTIVE_HEAP.with(|active| {
            active.replace(self.0.take());
        });
    }
}
impl RealmHeap {
    pub(super) fn exhausted(&mut self) -> bool {
        if self.exhausted {
            self.prune_expired();
            self.exhausted = self.objects.len() + self.environments.len() > 131_072;
        }
        self.exhausted
    }
    fn prune_expired(&mut self) {
        self.objects.retain(|object| object.strong_count() > 0);
        self.environments
            .retain(|environment| environment.strong_count() > 0);
    }
    pub(super) fn release(&mut self) {
        let objects = self
            .objects
            .drain(..)
            .filter_map(|object| object.upgrade())
            .collect::<Vec<_>>();
        let environments = self
            .environments
            .drain(..)
            .filter_map(|environment| environment.upgrade())
            .collect::<Vec<_>>();
        for object in &objects {
            if let Ok(mut object) = object.try_borrow_mut() {
                object.props.clear();
                object.indexed.clear();
                object.prototype = None;
                object.function = None;
                object.payload = None;
            }
        }
        for environment in &environments {
            if let Ok(mut environment) = environment.try_borrow_mut() {
                environment.bindings.clear();
                environment.parent = None;
            }
        }
    }
}
fn track_object(object: &Rc<RefCell<Object>>) {
    ACTIVE_HEAP.with(|active| {
        if let Some(heap) = active.borrow().as_ref().and_then(std::rc::Weak::upgrade) {
            let mut heap = heap.borrow_mut();
            object.borrow_mut().realm = Some(heap.realm);
            heap.objects.push(Rc::downgrade(object));
            if heap.objects.len() + heap.environments.len() > 131_072 {
                heap.prune_expired();
                heap.exhausted = heap.objects.len() + heap.environments.len() > 131_072;
            }
        }
    });
}
fn track_environment(environment: &Env) {
    ACTIVE_HEAP.with(|active| {
        if let Some(heap) = active.borrow().as_ref().and_then(std::rc::Weak::upgrade) {
            let mut heap = heap.borrow_mut();
            heap.environments.push(Rc::downgrade(environment));
            if heap.objects.len() + heap.environments.len() > 131_072 {
                heap.prune_expired();
                heap.exhausted = heap.objects.len() + heap.environments.len() > 131_072;
            }
        }
    });
}

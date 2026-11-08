use super::{Object, Value, builtins::number};
use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;

pub(super) struct ArrayBuffer(RefCell<Option<Vec<u8>>>);

impl ArrayBuffer {
    pub(super) fn new_value(bytes: Option<Vec<u8>>) -> Value {
        let value = Value::object("ArrayBuffer");
        value.as_object().unwrap().borrow_mut().payload = Some(Rc::new(Self(RefCell::new(bytes))));
        value
    }

    pub(super) fn of(value: &Value) -> Option<Rc<Self>> {
        payload(value)
    }

    pub(super) fn bytes(&self) -> Option<Vec<u8>> {
        self.0.borrow().clone()
    }

    pub(super) fn len(&self) -> Option<usize> {
        self.0.borrow().as_ref().map(Vec::len)
    }

    pub(super) fn transfer_to(&self, target: &Self) {
        *target.0.borrow_mut() = self.0.borrow_mut().take();
    }
}

pub(super) struct TypedArray {
    pub(super) buffer: Value,
    pub(super) offset: usize,
    pub(super) count: usize,
    backing: Rc<ArrayBuffer>,
    kind: ElementKind,
}

impl TypedArray {
    pub(super) fn new_value(name: &str, buffer: Value, offset: usize, count: usize) -> Value {
        let view = Self {
            backing: ArrayBuffer::of(&buffer).expect("typed array backing must be an ArrayBuffer"),
            kind: ElementKind::from_name(name).expect("supported typed array constructor"),
            buffer,
            offset,
            count,
        };
        let value = Value::object(name);
        value.as_object().unwrap().borrow_mut().payload = Some(Rc::new(view));
        value
    }

    pub(super) fn of(value: &Value) -> Option<Rc<Self>> {
        payload(value)
    }

    pub(super) fn width(&self) -> usize {
        self.kind.width()
    }

    pub(super) fn len(&self) -> Option<usize> {
        self.backing.len().map(|_| self.count)
    }

    pub(super) fn bytes(&self) -> Option<Vec<u8>> {
        self.backing
            .0
            .borrow()
            .as_ref()
            .map(|bytes| bytes[self.offset..self.offset + self.count * self.width()].to_vec())
    }

    pub(super) fn get(&self, index: usize) -> Option<Value> {
        let bytes = self.backing.0.borrow();
        let bytes = bytes.as_ref()?;
        if index >= self.count {
            return None;
        }
        let start = self.offset + index * self.width();
        Some(self.kind.read(&bytes[start..start + self.width()]))
    }

    pub(super) fn set(&self, index: usize, value: &Value) {
        if index >= self.count {
            return;
        }
        let encoded = self.kind.encode(value);
        let mut bytes = self.backing.0.borrow_mut();
        if let Some(bytes) = bytes.as_mut() {
            let start = self.offset + index * self.width();
            bytes[start..start + self.width()].copy_from_slice(&encoded[..self.width()]);
        }
    }

    fn items(&self) -> Vec<Value> {
        let bytes = self.backing.0.borrow();
        bytes.as_ref().map_or_else(Vec::new, |bytes| {
            bytes[self.offset..self.offset + self.count * self.width()]
                .chunks_exact(self.width())
                .map(|bytes| self.kind.read(bytes))
                .collect()
        })
    }
}

fn payload<T: Any>(value: &Value) -> Option<Rc<T>> {
    value.as_object()?.borrow().payload.clone()?.downcast().ok()
}

impl Object {
    pub(super) fn binary_index(&self, key: &str) -> Option<usize> {
        let payload = self.payload.as_deref()?;
        if !payload.is::<TypedArray>() && !payload.is::<ArrayBuffer>() {
            return None;
        }
        key.parse().ok()
    }

    pub(super) fn binary_property(&self, key: &str) -> Option<Value> {
        let payload = self.payload.as_deref()?;
        if let Some(view) = payload.downcast_ref::<TypedArray>() {
            return match key {
                "buffer" => Some(view.buffer.clone()),
                "length" => Some(Value::Number(view.len().unwrap_or(0) as f64)),
                "byteLength" => Some(Value::Number(
                    (view.len().unwrap_or(0) * view.width()) as f64,
                )),
                "byteOffset" => Some(Value::Number(view.len().map_or(0, |_| view.offset) as f64)),
                _ => key
                    .parse()
                    .ok()
                    .map(|index| view.get(index).unwrap_or_default()),
            };
        }
        let buffer = payload.downcast_ref::<ArrayBuffer>()?;
        if key == "byteLength" {
            return Some(Value::Number(buffer.len().unwrap_or(0) as f64));
        }
        key.parse()
            .ok()
            .map(|index| buffer_index(buffer, index).unwrap_or_default())
    }

    pub(super) fn indexed_len(&self) -> usize {
        if let Some(payload) = self.payload.as_deref() {
            if let Some(view) = payload.downcast_ref::<TypedArray>() {
                return view.len().unwrap_or(0);
            }
            if let Some(buffer) = payload.downcast_ref::<ArrayBuffer>() {
                return buffer.len().unwrap_or(0);
            }
        }
        self.indexed.len()
    }

    pub(super) fn indexed_value(&self, index: usize) -> Option<Value> {
        if let Some(payload) = self.payload.as_deref() {
            if let Some(view) = payload.downcast_ref::<TypedArray>() {
                return view.get(index);
            }
            if let Some(buffer) = payload.downcast_ref::<ArrayBuffer>() {
                return buffer_index(buffer, index);
            }
        }
        self.indexed.get(index).cloned()
    }

    pub(super) fn binary_items(&self) -> Option<Vec<Value>> {
        let payload = self.payload.as_deref()?;
        if let Some(view) = payload.downcast_ref::<TypedArray>() {
            return Some(view.items());
        }
        let buffer = payload.downcast_ref::<ArrayBuffer>()?;
        Some(buffer.0.borrow().as_ref().map_or_else(Vec::new, |bytes| {
            bytes
                .iter()
                .map(|&byte| Value::Number(f64::from(byte)))
                .collect()
        }))
    }
}

pub(super) fn define_index(target: &Value, key: &str, desc: &Value) -> Option<bool> {
    let object = target.as_object()?;
    let (index, frozen) = {
        let object = object.borrow();
        (object.binary_index(key)?, object.frozen)
    };
    let Some(descriptor) = desc.as_object() else {
        return Some(false);
    };
    let (value, valid) = {
        let descriptor = descriptor.borrow();
        let has_false_flag = ["writable", "enumerable", "configurable"]
            .iter()
            .any(|&flag| {
                descriptor
                    .props
                    .get(flag)
                    .is_some_and(|property| !property.value.truthy())
            });
        (
            descriptor
                .props
                .get("value")
                .map(|property| property.value.clone()),
            !has_false_flag
                && !descriptor.props.contains_key("get")
                && !descriptor.props.contains_key("set"),
        )
    };
    if frozen || !valid || index >= object.borrow().indexed_len() {
        return Some(false);
    }
    if let Some(value) = value {
        target.put(key, value);
    }
    Some(true)
}

fn buffer_index(buffer: &ArrayBuffer, index: usize) -> Option<Value> {
    let byte = *buffer.0.borrow().as_ref()?.get(index)?;
    Some(Value::Number(f64::from(byte)))
}

pub(super) fn set_property(payload: &dyn Any, key: &str, value: &Value) -> bool {
    if let Some(view) = payload.downcast_ref::<TypedArray>() {
        if let Ok(index) = key.parse() {
            view.set(index, value);
            return true;
        }
        return matches!(key, "buffer" | "length" | "byteLength" | "byteOffset");
    }
    if let Some(buffer) = payload.downcast_ref::<ArrayBuffer>() {
        if let Ok(index) = key.parse::<usize>() {
            let byte = number(value).clamp(0.0, 255.0) as u8;
            if let Some(slot) = buffer
                .0
                .borrow_mut()
                .as_mut()
                .and_then(|bytes| bytes.get_mut(index))
            {
                *slot = byte;
            }
            return true;
        }
        return key == "byteLength";
    }
    false
}

#[derive(Clone, Copy)]
pub(super) enum ElementKind {
    Uint8,
    Uint8Clamped,
    Int8,
    Uint16,
    Int16,
    Uint32,
    Int32,
    Float32,
    Float64,
}

impl ElementKind {
    pub(super) fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "Uint8Array" => Self::Uint8,
            "Uint8ClampedArray" => Self::Uint8Clamped,
            "Int8Array" => Self::Int8,
            "Uint16Array" => Self::Uint16,
            "Int16Array" => Self::Int16,
            "Uint32Array" => Self::Uint32,
            "Int32Array" => Self::Int32,
            "Float32Array" => Self::Float32,
            "Float64Array" => Self::Float64,
            _ => return None,
        })
    }

    pub(super) fn width(self) -> usize {
        match self {
            Self::Uint8 | Self::Uint8Clamped | Self::Int8 => 1,
            Self::Uint16 | Self::Int16 => 2,
            Self::Uint32 | Self::Int32 | Self::Float32 => 4,
            Self::Float64 => 8,
        }
    }

    fn encode(self, value: &Value) -> [u8; 8] {
        let n = number(value);
        let mut bytes = [0; 8];
        match self {
            Self::Uint8 | Self::Int8 => bytes[0] = wrap_integer(n, 8) as u8,
            Self::Uint8Clamped => bytes[0] = n.clamp(0.0, 255.0).round_ties_even() as u8,
            Self::Uint16 | Self::Int16 => {
                bytes[..2].copy_from_slice(&(wrap_integer(n, 16) as u16).to_le_bytes());
            }
            Self::Uint32 | Self::Int32 => {
                bytes[..4].copy_from_slice(&wrap_integer(n, 32).to_le_bytes());
            }
            Self::Float32 => bytes[..4].copy_from_slice(&(n as f32).to_le_bytes()),
            Self::Float64 => bytes = n.to_le_bytes(),
        }
        bytes
    }

    pub(super) fn append(self, bytes: &mut Vec<u8>, value: &Value) {
        bytes.extend_from_slice(&self.encode(value)[..self.width()]);
    }

    fn read(self, bytes: &[u8]) -> Value {
        Value::Number(match self {
            Self::Uint8 | Self::Uint8Clamped => f64::from(bytes[0]),
            Self::Int8 => f64::from(bytes[0] as i8),
            Self::Uint16 => f64::from(u16::from_le_bytes(bytes.try_into().unwrap())),
            Self::Int16 => f64::from(i16::from_le_bytes(bytes.try_into().unwrap())),
            Self::Uint32 => f64::from(u32::from_le_bytes(bytes.try_into().unwrap())),
            Self::Int32 => f64::from(i32::from_le_bytes(bytes.try_into().unwrap())),
            Self::Float32 => f64::from(f32::from_le_bytes(bytes.try_into().unwrap())),
            Self::Float64 => f64::from_le_bytes(bytes.try_into().unwrap()),
        })
    }
}

fn wrap_integer(value: f64, bits: u32) -> u32 {
    if !value.is_finite() {
        return 0;
    }
    let modulus = (1_u64 << bits) as f64;
    let remainder = value.trunc() % modulus;
    if remainder < 0.0 {
        (remainder + modulus) as u32
    } else {
        remainder as u32
    }
}

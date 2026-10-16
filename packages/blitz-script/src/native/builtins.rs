use super::*;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

const MAX_COLLECTION: usize = 1_000_000;
static NEXT_BOUND_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_SYMBOL_ID: AtomicU64 = AtomicU64::new(1);

fn native(name: &str) -> Value {
    Value::native(name)
}

fn property_key(value: &Value) -> String {
    super::exec::property_key(value)
}

fn string(v: &Value) -> String {
    match v {
        Value::Undefined => "undefined".into(),
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) if n.is_nan() => "NaN".into(),
        Value::Number(n) if *n == f64::INFINITY => "Infinity".into(),
        Value::Number(n) if *n == f64::NEG_INFINITY => "-Infinity".into(),
        Value::Number(n) if *n == 0.0 => "0".into(),
        Value::Number(n) => n.to_string(),
        Value::String(_) => v.text(),
        Value::Object(_) if v.kind() == "Array" => {
            v.items().iter().map(string).collect::<Vec<_>>().join(",")
        }
        Value::Object(_) if v.kind() == "Function" || v.kind() == "Native" => {
            "function () { [native code] }".into()
        }
        Value::Object(_) => v.text(),
    }
}

fn number(v: &Value) -> f64 {
    match v {
        Value::Undefined => f64::NAN,
        Value::Null => 0.0,
        Value::Bool(false) => 0.0,
        Value::Bool(true) => 1.0,
        Value::Number(n) => *n,
        Value::String(_) => {
            let s = v.text();
            if s.trim().is_empty() {
                0.0
            } else {
                s.trim().parse().unwrap_or(f64::NAN)
            }
        }
        Value::Object(_) => number(&Value::string(string(v))),
    }
}

fn integer(v: &Value) -> i64 {
    let n = number(v);
    if !n.is_finite() || n == 0.0 {
        0
    } else {
        n.trunc().clamp(i64::MIN as f64, i64::MAX as f64) as i64
    }
}

fn number_text(value: f64) -> String {
    if value.is_nan() {
        "NaN".into()
    } else if value == f64::INFINITY {
        "Infinity".into()
    } else if value == f64::NEG_INFINITY {
        "-Infinity".into()
    } else {
        value.to_string()
    }
}

fn exponential_text(value: f64, fraction_digits: usize) -> String {
    let rendered = format!("{value:.fraction_digits$e}");
    let Some((mantissa, exponent)) = rendered.split_once('e') else {
        return rendered;
    };
    let exponent: i32 = exponent.parse().unwrap_or(0);
    format!("{mantissa}e{exponent:+}")
}

fn shortest_exponential_text(value: f64) -> String {
    let rendered = format!("{value:e}");
    let Some((mantissa, exponent)) = rendered.split_once('e') else {
        return rendered;
    };
    let exponent: i32 = exponent.parse().unwrap_or(0);
    format!("{mantissa}e{exponent:+}")
}

fn fixed_number_text(value: f64, fraction_digits: usize) -> String {
    if value == 0.0 {
        return format!("{:.fraction_digits$}", 0.0);
    }
    if value.abs() >= 1e21 {
        return number_text(value);
    }
    format!("{value:.fraction_digits$}")
}

fn precision_number_text(value: f64, precision: usize) -> String {
    if !value.is_finite() {
        return number_text(value);
    }
    if value == 0.0 {
        return format!("{:.digits$}", 0.0, digits = precision - 1);
    }
    let exponent = value.abs().log10().floor() as i32;
    if exponent < -6 || exponent >= precision as i32 {
        exponential_text(value, precision - 1)
    } else {
        let fraction_digits = (precision as i32 - exponent - 1).max(0) as usize;
        format!("{value:.fraction_digits$}")
    }
}

fn number_digits(value: &Value, default: i64, min: i64, max: i64) -> Result<usize> {
    let raw = number(value);
    let digits = if raw.is_nan() || raw == 0.0 {
        0
    } else if raw.is_finite() {
        raw.trunc() as i64
    } else {
        return Err(Error::new(
            "RangeError",
            "digit argument is outside the supported range",
        ));
    };
    let digits = if matches!(value, Value::Undefined) {
        default
    } else {
        digits
    };
    if !(min..=max).contains(&digits) {
        return Err(Error::new(
            "RangeError",
            "digit argument is outside the supported range",
        ));
    }
    Ok(digits as usize)
}

fn index(v: &Value, len: usize) -> usize {
    let n = integer(v);
    if n < 0 {
        (len as i64 + n).max(0) as usize
    } else {
        (n as usize).min(len)
    }
}

fn args_string(args: &[Value], at: usize, default: &str) -> String {
    args.get(at).map(string).unwrap_or_else(|| default.into())
}

fn array_like(v: &Value) -> Vec<Value> {
    if matches!(v, Value::String(_)) {
        return v
            .text()
            .encode_utf16()
            .map(|c| Value::string(String::from_utf16_lossy(&[c])))
            .collect();
    }
    if matches!(v, Value::Object(_)) {
        let items = v.items();
        if v.kind() == "Map" {
            return map_entries(v)
                .into_iter()
                .map(|(k, val)| Value::array(vec![k, val]))
                .collect();
        }
        if !items.is_empty()
            || v.kind() == "Array"
            || v.kind() == "NodeList"
            || v.kind().ends_with("Array")
        {
            return items;
        }
        let len = integer(&v.get("length")).clamp(0, MAX_COLLECTION as i64) as usize;
        return (0..len).map(|i| v.get(&i.to_string())).collect();
    }
    Vec::new()
}

fn array_from_utf16(text: &str) -> Vec<Value> {
    text.encode_utf16()
        .map(|unit| Value::String(vec![unit].into()))
        .collect()
}

fn string_units(value: &Value) -> Vec<u16> {
    if let Value::String(units) = value {
        units.to_vec()
    } else {
        string(value).encode_utf16().collect()
    }
}

fn find_units(haystack: &[u16], needle: &[u16], start: usize, end: usize) -> Option<usize> {
    if needle.is_empty() {
        return Some(start.min(end));
    }
    let end = end.min(haystack.len());
    if needle.len() > end {
        return None;
    }
    let last = end - needle.len();
    if start > last {
        return None;
    }
    (start..=last).find(|index| &haystack[*index..*index + needle.len()] == needle)
}

fn format_locale_number(value: &Value, locale: &Value) -> String {
    let number = number(value);
    if !number.is_finite() {
        return string(&Value::Number(number));
    }
    let locale = string(locale);
    let (group, decimal) = if locale.starts_with("fr") {
        ("\u{202f}", ",")
    } else if locale.starts_with("de")
        || locale.starts_with("es")
        || locale.starts_with("it")
        || locale.starts_with("pt")
    {
        (".", ",")
    } else {
        (",", ".")
    };
    let raw = number.to_string();
    let (sign, raw) = raw
        .strip_prefix('-')
        .map_or(("", raw.as_str()), |rest| ("-", rest));
    let mut parts = raw.split('.');
    let integer = parts.next().unwrap_or("0");
    let fraction = parts.next();
    let mut grouped = String::new();
    for (index, digit) in integer.chars().enumerate() {
        if index > 0 && (integer.len() - index) % 3 == 0 {
            grouped.push_str(group);
        }
        grouped.push(digit);
    }
    format!(
        "{sign}{grouped}{}{}",
        if fraction.is_some() { decimal } else { "" },
        fraction.unwrap_or("")
    )
}

#[derive(Clone)]
struct BoundCall {
    target: Value,
    this: Value,
    args: Vec<Value>,
}
#[derive(Clone)]
struct IteratorState {
    values: Vec<Value>,
    cursor: usize,
}

fn bound_call(value: &Value) -> Option<BoundCall> {
    value
        .as_object()?
        .borrow()
        .payload
        .as_ref()?
        .downcast_ref::<BoundCall>()
        .cloned()
}

fn iterator_state(value: &Value) -> Option<IteratorState> {
    value
        .as_object()?
        .borrow()
        .payload
        .as_ref()?
        .downcast_ref::<IteratorState>()
        .cloned()
}

fn make_iterator(values: Vec<Value>) -> Value {
    let iterator = Value::object("Iterator");
    if let Some(object) = iterator.as_object() {
        object.borrow_mut().payload = Some(Rc::new(IteratorState { values, cursor: 0 }));
    }
    iterator
}

fn json_value(
    value: &Value,
    seen: &mut Vec<usize>,
    depth: usize,
    array_item: bool,
) -> Result<serde_json::Value> {
    if depth > 128 {
        return Err(Error::new("RangeError", "JSON nesting limit exceeded"));
    }
    Ok(match value {
        Value::Undefined if array_item => serde_json::Value::Null,
        Value::Undefined => return Err(Error::type_error("undefined object member")),
        Value::Null => serde_json::Value::Null,
        Value::Bool(v) => serde_json::Value::Bool(*v),
        Value::Number(v) => serde_json::Number::from_f64(*v)
            .map_or(serde_json::Value::Null, serde_json::Value::Number),
        Value::String(v) => serde_json::Value::String(String::from_utf16_lossy(v)),
        Value::Object(object) => {
            let id = Rc::as_ptr(object) as usize;
            if seen.contains(&id) {
                return Err(Error::type_error(
                    "cyclic value cannot be represented as JSON",
                ));
            }
            seen.push(id);
            let (kind, items, props) = {
                let object = object.borrow();
                (
                    object.kind.clone(),
                    object.indexed.clone(),
                    object.props.clone(),
                )
            };
            let out = if matches!(kind.as_str(), "Array" | "NodeList" | "HTMLCollection") {
                serde_json::Value::Array(
                    items
                        .iter()
                        .map(|item| json_value(item, seen, depth + 1, true))
                        .collect::<Result<_>>()?,
                )
            } else {
                let mut map = serde_json::Map::new();
                for (key, property) in props {
                    if !property.enumerable
                        || matches!(&property.value, Value::Undefined)
                        || matches!(property.value.kind().as_str(), "Native" | "Function")
                    {
                        continue;
                    }
                    map.insert(key, json_value(&property.value, seen, depth + 1, false)?);
                }
                serde_json::Value::Object(map)
            };
            seen.pop();
            out
        }
    })
}

impl<B: RuntimeBridge> Guest<B> {
    fn register_symbol_key(&self, symbol: &Value) {
        let key = super::exec::property_key(symbol);
        self.global.get("\0symbol_keys").put(&key, symbol.clone());
    }

    pub(super) fn cached_native(&mut self, name: &str) -> Value {
        let key = "\0builtin_method_cache";
        let mut cache = self.global.get(key);
        if !matches!(cache, Value::Object(_)) {
            cache = Value::object("NativeMethodCache");
            if let Some(global) = self.global.as_object() {
                global.borrow_mut().props.insert(
                    key.into(),
                    Property {
                        value: cache.clone(),
                        get: None,
                        set: None,
                        enumerable: false,
                    },
                );
            }
        }
        let value = cache.get(name);
        if value.kind() != "Undefined" {
            return value;
        }
        let value = native(name);
        cache.put(name, value.clone());
        value
    }

    fn iterable_to_vec(&mut self, doc: &mut BaseDocument, source: &Value) -> Result<Vec<Value>> {
        let iterator_method = self.get_property(doc, source, "@@iterator")?;
        if matches!(iterator_method, Value::Undefined) {
            return Ok(array_like(source));
        }
        let iterator = self.call(doc, iterator_method, source.clone(), Vec::new(), false)?;
        let mut values = Vec::new();
        for _ in 0..MAX_COLLECTION {
            let next = self.get_property(doc, &iterator, "next")?;
            let step = self.call(doc, next, iterator.clone(), Vec::new(), false)?;
            if self.get_property(doc, &step, "done")?.truthy() {
                return Ok(values);
            }
            values.push(self.get_property(doc, &step, "value")?);
        }
        Err(Error::new("RangeError", "iterator exceeded element limit"))
    }

    pub(super) fn install_builtins(&mut self) {
        self.global
            .put_hidden("\0symbol_keys", Value::object("SymbolKeyRegistry"));
        for name in [
            "Object",
            "Array",
            "String",
            "Number",
            "Boolean",
            "Function",
            "Map",
            "Set",
            "ArrayBuffer",
            "Uint8Array",
            "Uint8ClampedArray",
            "Uint16Array",
            "Uint32Array",
            "Int8Array",
            "Int16Array",
            "Int32Array",
            "Float32Array",
            "Float64Array",
            "Error",
            "TypeError",
            "RangeError",
            "ReferenceError",
            "SyntaxError",
            "JSON",
            "Math",
            "Reflect",
            "Symbol",
            "Proxy",
        ] {
            self.global.put(name, native(name));
        }
        let symbol_iterator = Value::object("Symbol");
        for name in ["parseFloat", "parseInt", "isFinite", "isNaN"] {
            self.global.put(name, native(name));
        }
        if let Some(object) = symbol_iterator.as_object() {
            object.borrow_mut().payload = Some(Rc::new(String::from("Symbol.iterator")));
        }
        self.global.get("Symbol").put("iterator", symbol_iterator);
        self.register_symbol_key(&self.global.get("Symbol").get("iterator"));
        let symbol_tag = Value::object("Symbol");
        if let Some(object) = symbol_tag.as_object() {
            object.borrow_mut().payload = Some(Rc::new(String::from("Symbol.toStringTag")));
        }
        self.global.get("Symbol").put("toStringTag", symbol_tag);
        self.register_symbol_key(&self.global.get("Symbol").get("toStringTag"));
        if let Some(global) = self.global.as_object() {
            global.borrow_mut().props.insert(
                "\0bound_function_registry".into(),
                Property {
                    value: Value::object("BoundFunctionRegistry"),
                    get: None,
                    set: None,
                    enumerable: false,
                },
            );
            global.borrow_mut().props.insert(
                "\0symbol_registry".into(),
                Property {
                    value: Value::object("SymbolRegistry"),
                    get: None,
                    set: None,
                    enumerable: false,
                },
            );
            global.borrow_mut().props.insert(
                "\0proxy_revokers".into(),
                Property {
                    value: Value::object("ProxyRevokerRegistry"),
                    get: None,
                    set: None,
                    enumerable: false,
                },
            );
        }
        let object_proto = Value::object("Object");
        let array_proto = Value::array(Vec::new());
        let string_proto = Value::string("");
        let function_proto = Value::native("Function");
        for (name, proto) in [
            ("Object", object_proto),
            ("Array", array_proto),
            ("String", Value::object("StringPrototype")),
            ("Function", function_proto),
        ] {
            if let Some(obj) = self.global.get(name).as_object() {
                obj.borrow_mut()
                    .props
                    .insert("prototype".into(), Property::data(proto));
            }
        }
        let object_prototype = self.global.get("Object").get("prototype");
        let error_prototype = Value::object("Error");
        if let Some(object) = error_prototype.as_object() {
            object.borrow_mut().prototype = Some(object_prototype);
        }
        let error_constructor = self.global.get("Error");
        error_constructor.put("prototype", error_prototype.clone());
        error_prototype.put("constructor", error_constructor);
        for name in ["TypeError", "RangeError", "ReferenceError", "SyntaxError"] {
            let prototype = Value::object("Error");
            if let Some(object) = prototype.as_object() {
                object.borrow_mut().prototype = Some(error_prototype.clone());
            }
            let constructor = self.global.get(name);
            constructor.put("prototype", prototype.clone());
            prototype.put("constructor", constructor);
        }
        let json = Value::object("JSON");
        json.put("parse", native("JSON.parse"));
        json.put("stringify", native("JSON.stringify"));
        self.global.put("JSON", json);
        let math = Value::object("Math");
        for key in [
            "abs", "ceil", "floor", "round", "trunc", "max", "min", "pow", "sqrt", "cbrt", "sign",
            "log", "log2", "log10", "exp", "sin", "cos", "tan", "asin", "acos", "atan", "atan2",
            "random", "hypot", "imul", "clz32",
        ] {
            math.put(key, native(&format!("Math.{key}")));
        }
        for (key, n) in [
            ("PI", std::f64::consts::PI),
            ("E", std::f64::consts::E),
            ("LN2", std::f64::consts::LN_2),
            ("LN10", std::f64::consts::LN_10),
            ("SQRT2", std::f64::consts::SQRT_2),
        ] {
            math.put(key, Value::Number(n));
        }
        self.global.put("Math", math);
        self.global.put("NaN", Value::Number(f64::NAN));
        self.global.put("Infinity", Value::Number(f64::INFINITY));
        let _ = string_proto;
    }

    pub(super) fn builtin_get(
        &mut self,
        _doc: &mut BaseDocument,
        receiver: &Value,
        key: &str,
    ) -> Result<Option<Value>> {
        let kind = receiver.kind();
        let own_native = receiver.get("__native").text();
        if !own_native.is_empty() {
            let methods: &[&str] = match own_native.as_str() {
                "Object" => &[
                    "keys",
                    "values",
                    "entries",
                    "assign",
                    "create",
                    "defineProperty",
                    "defineProperties",
                    "getOwnPropertyDescriptors",
                    "getOwnPropertyDescriptor",
                    "getOwnPropertyNames",
                    "getOwnPropertySymbols",
                    "hasOwn",
                    "is",
                    "isExtensible",
                    "preventExtensions",
                    "freeze",
                    "isFrozen",
                    "seal",
                    "isSealed",
                    "getPrototypeOf",
                    "setPrototypeOf",
                ],
                "Array" => &["isArray", "from", "of"],
                "String" => &["fromCharCode", "fromCodePoint", "raw"],
                "Number" => &[
                    "isFinite",
                    "isInteger",
                    "isNaN",
                    "isSafeInteger",
                    "parseFloat",
                    "parseInt",
                ],
                "Boolean" => &[],
                "JSON" => &["parse", "stringify"],
                "Map" => &["groupBy"],
                "Set" => &[],
                "Symbol" => &["for", "keyFor"],
                "Proxy" => &["revocable"],
                "Reflect" => &[
                    "get",
                    "set",
                    "has",
                    "ownKeys",
                    "apply",
                    "construct",
                    "getOwnPropertyDescriptor",
                    "defineProperty",
                    "deleteProperty",
                    "getPrototypeOf",
                    "setPrototypeOf",
                    "isExtensible",
                    "preventExtensions",
                ],
                _ => &[],
            };
            if methods.contains(&key) {
                return Ok(Some(self.cached_native(&format!("{own_native}.{key}"))));
            }
        }
        let names: &[&str] = match kind.as_str() {
            "Array" | "NodeList" => &[
                "at",
                "concat",
                "copyWithin",
                "entries",
                "every",
                "fill",
                "filter",
                "find",
                "findIndex",
                "findLast",
                "findLastIndex",
                "forEach",
                "includes",
                "indexOf",
                "join",
                "keys",
                "lastIndexOf",
                "map",
                "pop",
                "push",
                "reduce",
                "reduceRight",
                "reverse",
                "shift",
                "slice",
                "some",
                "sort",
                "splice",
                "toReversed",
                "toSorted",
                "toSpliced",
                "unshift",
                "values",
                "with",
                "flat",
                "flatMap",
            ],
            "String" | "StringPrototype" => &[
                "at",
                "charAt",
                "charCodeAt",
                "codePointAt",
                "concat",
                "endsWith",
                "includes",
                "indexOf",
                "lastIndexOf",
                "match",
                "normalize",
                "padEnd",
                "padStart",
                "repeat",
                "replace",
                "replaceAll",
                "search",
                "slice",
                "split",
                "startsWith",
                "substr",
                "substring",
                "toLocaleString",
                "toLowerCase",
                "toUpperCase",
                "trim",
                "trimStart",
                "trimEnd",
                "valueOf",
                "toString",
            ],
            "Map" => &[
                "clear", "delete", "entries", "forEach", "get", "has", "keys", "set", "values",
            ],
            "Set" => &[
                "add", "clear", "delete", "entries", "forEach", "has", "keys", "values",
            ],
            "Uint8Array" | "Uint8ClampedArray" | "Uint16Array" | "Uint32Array" | "Int8Array"
            | "Int16Array" | "Int32Array" | "Float32Array" | "Float64Array" => &[
                "at",
                "copyWithin",
                "entries",
                "every",
                "fill",
                "filter",
                "find",
                "findIndex",
                "forEach",
                "includes",
                "indexOf",
                "join",
                "keys",
                "lastIndexOf",
                "map",
                "reduce",
                "reduceRight",
                "reverse",
                "set",
                "slice",
                "some",
                "sort",
                "subarray",
                "values",
                "toString",
            ],
            "Function" | "Native" | "BoundFunction" => &["apply", "bind", "call", "toString"],
            "Number" => &[
                "toExponential",
                "toFixed",
                "toLocaleString",
                "toPrecision",
                "toString",
                "valueOf",
            ],
            "Error" | "TypeError" | "RangeError" | "ReferenceError" | "SyntaxError" => {
                &["toString"]
            }
            "Object" => &[
                "toString",
                "valueOf",
                "hasOwnProperty",
                "propertyIsEnumerable",
                "isPrototypeOf",
                "__defineGetter__",
                "__defineSetter__",
                "__lookupGetter__",
                "__lookupSetter__",
            ],
            _ => &[],
        };
        if names.contains(&key) {
            let owner = match kind.as_str() {
                "BoundFunction" | "Native" => "Function",
                "String" | "StringPrototype" => "String.prototype",
                "Number" => "Number.prototype",
                "Object" => "Object.prototype",
                _ => kind.as_str(),
            };
            return Ok(Some(self.cached_native(&format!("{owner}.{key}"))));
        }
        if let Ok(index) = key.parse::<usize>() {
            if matches!(receiver, Value::Object(_))
                && (kind == "Array"
                    || kind == "NodeList"
                    || kind == "ArrayBuffer"
                    || kind.ends_with("Array"))
            {
                return Ok(Some(
                    receiver
                        .items()
                        .get(index)
                        .cloned()
                        .unwrap_or(Value::Undefined),
                ));
            }
        }
        if key == "@@iterator"
            && (kind == "Array"
                || kind == "Arguments"
                || kind == "NodeList"
                || kind.ends_with("Array")
                || kind == "Map"
                || kind == "Set"
                || matches!(receiver, Value::String(_)))
        {
            return Ok(Some(self.cached_native("Iterator.create")));
        }
        if kind == "Iterator" && key == "next" {
            return Ok(Some(self.cached_native("Iterator.next")));
        }
        if key == "length" {
            return match kind.as_str() {
                kind if kind.ends_with("Array")
                    || matches!(kind, "Arguments" | "NodeList" | "HTMLCollection") =>
                {
                    Ok(Some(Value::Number(receiver.items().len() as f64)))
                }
                "String" | "StringPrototype" => Ok(Some(Value::Number(
                    receiver.text().encode_utf16().count() as f64,
                ))),
                _ => Ok(None),
            };
        }
        if key == "size" && (kind == "Map" || kind == "Set") {
            return Ok(Some(Value::Number(receiver.items().len() as f64)));
        }
        if receiver.as_object().is_some()
            && matches!(
                key,
                "isPrototypeOf"
                    | "hasOwnProperty"
                    | "propertyIsEnumerable"
                    | "__lookupGetter__"
                    | "__lookupSetter__"
                    | "toString"
                    | "valueOf"
            )
        {
            return Ok(Some(self.cached_native(&format!("Object.prototype.{key}"))));
        }
        Ok(None)
    }

    pub(super) fn builtin_call(
        &mut self,
        doc: &mut BaseDocument,
        name: &str,
        this: Value,
        args: Vec<Value>,
        construct: bool,
    ) -> Result<Option<Value>> {
        let arg = |i: usize| args.get(i).cloned().unwrap_or(Value::Undefined);
        let a0 = arg(0);
        let a1 = arg(1);
        let result = match name {
            "Object" => Some(if matches!(a0, Value::Undefined | Value::Null) {
                Value::object("Object")
            } else {
                a0
            }),
            "Object.keys" | "Object.getOwnPropertyNames" => {
                let mut visible = Vec::new();
                for key in self.own_property_names(doc, &a0)? {
                    if super::exec::is_symbol_property_key(&self.global, &key) {
                        continue;
                    }
                    if name == "Object.keys"
                        && !self
                            .ordinary_descriptor(doc, &a0, &key)?
                            .get("enumerable")
                            .truthy()
                    {
                        continue;
                    }
                    visible.push(Value::string(key));
                }
                Some(Value::array(visible))
            }
            "Object.getOwnPropertySymbols" => Some(Value::array(
                self.own_property_names(doc, &a0)?
                    .into_iter()
                    .filter(|key| super::exec::is_symbol_property_key(&self.global, key))
                    .map(|key| super::exec::property_key_value(&self.global, &key))
                    .collect(),
            )),
            "Object.getOwnPropertyDescriptors" => {
                let descriptors = Value::object("Object");
                for key in self.own_property_names(doc, &a0)? {
                    let descriptor = self.ordinary_descriptor(doc, &a0, &key)?;
                    if !matches!(descriptor, Value::Undefined) {
                        let key_value = super::exec::property_key_value(&self.global, &key);
                        let key = property_key(&key_value);
                        self.set_property(doc, &descriptors, &key, descriptor)?;
                    }
                }
                Some(descriptors)
            }
            "Object.values" | "Object.entries" => {
                let keys = self.own_property_names(doc, &a0)?;
                let mut values = Vec::new();
                for key in keys {
                    if super::exec::is_symbol_property_key(&self.global, &key) {
                        continue;
                    }
                    let desc = self.ordinary_descriptor(doc, &a0, &key)?;
                    if !desc.get("enumerable").truthy() {
                        continue;
                    }
                    let value = self.get_property(doc, &a0, &key)?;
                    values.push(if name == "Object.values" {
                        value
                    } else {
                        Value::array(vec![Value::string(key), value])
                    });
                }
                Some(Value::array(values))
            }
            "Object.assign" => {
                if matches!(a0, Value::Null | Value::Undefined) {
                    return Err(Error::type_error(
                        "Object.assign target must not be null or undefined",
                    ));
                }
                let target = if matches!(a0, Value::Object(_)) {
                    a0.clone()
                } else {
                    Value::object("Object")
                };
                for source in args.iter().skip(1) {
                    if matches!(source, Value::Null | Value::Undefined) {
                        continue;
                    }
                    if let Value::String(units) = source {
                        for (index, unit) in units.iter().enumerate() {
                            self.set_property(
                                doc,
                                &target,
                                &index.to_string(),
                                Value::String(std::rc::Rc::from([*unit])),
                            )?;
                        }
                        continue;
                    }
                    if source.as_object().is_none() {
                        continue;
                    }
                    for key in self.own_property_names(doc, source)? {
                        let desc = self.ordinary_descriptor(doc, source, &key)?;
                        if desc.get("enumerable").truthy() {
                            let value = self.get_property(doc, source, &key)?;
                            self.set_property(doc, &target, &key, value)?;
                        }
                    }
                }
                Some(target)
            }
            "Object.create" => {
                if !matches!(a0, Value::Null | Value::Object(_)) {
                    return Err(Error::type_error(
                        "Object.create prototype must be an object or null",
                    ));
                }
                let out = Value::object("Object");
                if let Some(o) = out.as_object() {
                    o.borrow_mut().prototype = (!matches!(a0, Value::Null)).then_some(a0);
                }
                if !matches!(a1, Value::Undefined) {
                    self.call(
                        doc,
                        native("Object.defineProperties"),
                        Value::Undefined,
                        vec![out.clone(), a1],
                        false,
                    )?;
                }
                Some(out)
            }
            "Object.defineProperty" => {
                if !matches!(a0, Value::Object(_)) {
                    return Err(Error::type_error(
                        "Object.defineProperty target must be an object",
                    ));
                }
                let key = property_key(&a1);
                let desc = arg(2);
                let val = desc.get("value");
                let get = desc.get("get");
                let set = desc.get("set");
                if a0.kind() == "Proxy" {
                    if !self.proxy_define(doc, &a0, &key, &desc)?.unwrap_or(false) {
                        return Err(Error::type_error(
                            "Proxy defineProperty trap rejected the property",
                        ));
                    }
                } else if let Some(o) = a0.as_object() {
                    o.borrow_mut().props.insert(
                        key,
                        Property {
                            value: val,
                            get: (!matches!(get, Value::Undefined)).then_some(get),
                            set: (!matches!(set, Value::Undefined)).then_some(set),
                            enumerable: desc.get("enumerable").truthy(),
                        },
                    );
                }
                Some(a0)
            }
            "Object.defineProperties" => {
                for key in self.own_property_names(doc, &a1)? {
                    let descriptor = self.ordinary_descriptor(doc, &a1, &key)?;
                    if descriptor.get("enumerable").truthy() {
                        let descriptor = self.get_property(doc, &a1, &key)?;
                        let _ = self.call(
                            doc,
                            native("Object.defineProperty"),
                            Value::Undefined,
                            vec![
                                a0.clone(),
                                super::exec::property_key_value(&self.global, &key),
                                descriptor,
                            ],
                            false,
                        )?;
                    }
                }
                Some(a0)
            }
            "Object.getOwnPropertyDescriptor" => {
                Some(self.ordinary_descriptor(doc, &a0, &property_key(&a1))?)
            }
            "Object.hasOwn" => Some(Value::Bool(!matches!(
                self.ordinary_descriptor(doc, &a0, &property_key(&a1))?,
                Value::Undefined
            ))),
            "Object.prototype.hasOwnProperty" | "Object.hasOwnProperty" => {
                Some(Value::Bool(!matches!(
                    self.ordinary_descriptor(doc, &this, &property_key(&a0))?,
                    Value::Undefined
                )))
            }
            "Object.prototype.__lookupGetter__" | "Object.prototype.__lookupSetter__" => {
                if matches!(this, Value::Null | Value::Undefined) {
                    return Err(Error::type_error(
                        "Object.prototype.__lookupGetter__/__lookupSetter__ called on null or undefined",
                    ));
                }
                let key = property_key(&a0);
                let field = if name.ends_with("__lookupGetter__") {
                    "get"
                } else {
                    "set"
                };
                let mut current = this.clone();
                let mut remaining = 1_024usize;
                let accessor = loop {
                    let descriptor = self.ordinary_descriptor(doc, &current, &key)?;
                    if !matches!(descriptor, Value::Undefined) {
                        break descriptor.get(field);
                    }
                    let prototype = current
                        .as_object()
                        .and_then(|object| object.borrow().prototype.clone());
                    let Some(prototype) = prototype else {
                        break Value::Undefined;
                    };
                    if remaining == 0 {
                        return Err(Error::new(
                            "RangeError",
                            "prototype chain exceeds limit during accessor lookup",
                        ));
                    }
                    remaining -= 1;
                    current = prototype;
                };
                Some(accessor)
            }
            "Object.prototype.toString" => {
                let kind = if matches!(this, Value::Null) {
                    String::from("Null")
                } else if matches!(this, Value::Undefined) {
                    String::from("Undefined")
                } else if matches!(this.kind().as_str(), "Native" | "Function") {
                    String::from("Function")
                } else {
                    this.kind()
                };
                Some(Value::string(format!("[object {kind}]")))
            }
            "Object.prototype.valueOf" => Some(this.clone()),
            "Object.prototype.propertyIsEnumerable" => Some(Value::Bool(
                self.ordinary_descriptor(doc, &this, &property_key(&a0))?
                    .get("enumerable")
                    .truthy(),
            )),
            "Object.prototype.isPrototypeOf" => {
                let mut current = a0.as_object().and_then(|o| o.borrow().prototype.clone());
                let mut found = false;
                let mut budget = 128;
                while let Some(v) = current {
                    if v.same(&this) {
                        found = true;
                        break;
                    }
                    current = v.as_object().and_then(|o| o.borrow().prototype.clone());
                    budget -= 1;
                    if budget == 0 {
                        break;
                    }
                }
                Some(Value::Bool(found))
            }
            "Object.prototype.__defineGetter__" | "Object.prototype.__defineSetter__" => {
                let key = property_key(&a0);
                if let Some(o) = this.as_object() {
                    let mut obj = o.borrow_mut();
                    let prop = obj
                        .props
                        .entry(key)
                        .or_insert_with(|| Property::data(Value::Undefined));
                    if name.ends_with("Getter__") {
                        prop.get = Some(a1)
                    } else {
                        prop.set = Some(a1)
                    }
                }
                Some(Value::Undefined)
            }
            "Object.is" => Some(Value::Bool(same_value(&a0, &a1))),
            "Object.freeze" | "Object.seal" | "Object.preventExtensions" => {
                if let Some(o) = a0.as_object() {
                    o.borrow_mut().frozen = true;
                }
                Some(a0)
            }
            "Object.isFrozen" | "Object.isSealed" => Some(Value::Bool(
                a0.as_object().is_some_and(|o| o.borrow().frozen),
            )),
            "Object.isExtensible" => Some(Value::Bool(
                a0.as_object().is_some_and(|o| !o.borrow().frozen),
            )),
            "Object.getPrototypeOf" => Some(
                a0.as_object()
                    .and_then(|o| o.borrow().prototype.clone())
                    .unwrap_or(Value::Null),
            ),
            "Object.setPrototypeOf" => {
                if let Some(o) = a0.as_object() {
                    o.borrow_mut().prototype = Some(a1);
                }
                Some(a0)
            }
            "Array" | "Array.of" => {
                if name == "Array" && construct && args.len() == 1 && matches!(a0, Value::Number(_))
                {
                    let n = integer(&a0);
                    if n < 0 || n as usize > MAX_COLLECTION {
                        return Err(Error::new("RangeError", "invalid array length"));
                    }
                    Some(Value::array(vec![Value::Undefined; n as usize]))
                } else {
                    Some(Value::array(args))
                }
            }
            "Array.isArray" => Some(Value::Bool(super::proxy::is_array(&a0)?)),
            "Array.from" => {
                let mapper_this = arg(2);
                let mut out = Vec::new();
                for (i, v) in self.iterable_to_vec(doc, &a0)?.into_iter().enumerate() {
                    out.push(if matches!(a1, Value::Undefined) {
                        v
                    } else {
                        self.call(
                            doc,
                            a1.clone(),
                            mapper_this.clone(),
                            vec![v, Value::Number(i as f64)],
                            false,
                        )?
                    });
                }
                Some(Value::array(out))
            }
            "String" => Some(Value::string(
                if matches!(a0, Value::Undefined) && args.is_empty() {
                    "".into()
                } else {
                    string(&a0)
                },
            )),
            "Number" => Some(Value::Number(if args.is_empty() {
                0.0
            } else {
                number(&a0)
            })),
            "Proxy" => Some(super::proxy::proxy_construct(a0, a1)?),
            "Proxy.revocable" => {
                let proxy = super::proxy::proxy_construct(a0, a1)?;
                let result = Value::object("Object");
                result.put("proxy", proxy.clone());
                result.put("revoke", super::proxy::revoker(&self.global, proxy));
                Some(result)
            }
            revoke if revoke.starts_with("Proxy.revoke.") => {
                Some(super::proxy::call_revoke(&self.global, revoke)?)
            }
            "Reflect.get" => {
                if a0.as_object().is_none() {
                    return Err(Error::type_error("Reflect.get target must be an object"));
                }
                let receiver = args.get(2).cloned().unwrap_or_else(|| a0.clone());
                if a0.kind() == "Proxy" {
                    Some(
                        self.proxy_get_with_receiver(doc, &a0, &property_key(&a1), receiver)?
                            .unwrap_or(Value::Undefined),
                    )
                } else {
                    Some(self.get_property_with_receiver(doc, &a0, &property_key(&a1), receiver)?)
                }
            }
            "Reflect.set" => {
                if a0.as_object().is_none() {
                    return Err(Error::type_error("Reflect.set target must be an object"));
                }
                let receiver = args.get(3).cloned().unwrap_or_else(|| a0.clone());
                let result = if a0.kind() == "Proxy" {
                    self.proxy_set_with_receiver(doc, &a0, &property_key(&a1), arg(2), receiver)?
                } else {
                    match self.set_property(doc, &a0, &property_key(&a1), arg(2)) {
                        Ok(()) => true,
                        Err(error) if error.name == "TypeError" => false,
                        Err(error) => return Err(error),
                    }
                };
                Some(Value::Bool(result))
            }
            "Reflect.has" => Some(Value::Bool(self.has_property(
                doc,
                &a0,
                &property_key(&a1),
            )?)),
            "Reflect.ownKeys" => Some(Value::array(
                self.own_property_names(doc, &a0)?
                    .into_iter()
                    .map(|key| super::exec::property_key_value(&self.global, &key))
                    .collect(),
            )),
            "Reflect.getOwnPropertyDescriptor" => {
                Some(self.ordinary_descriptor(doc, &a0, &property_key(&a1))?)
            }
            "Reflect.defineProperty" => {
                let descriptor = arg(2);
                if a0.kind() == "Proxy" {
                    Some(Value::Bool(
                        self.proxy_define(doc, &a0, &property_key(&a1), &descriptor)?
                            .unwrap_or(false),
                    ))
                } else {
                    let key = property_key(&a1);
                    let value = descriptor.get("value");
                    let get = descriptor.get("get");
                    let set = descriptor.get("set");
                    if let Some(object) = a0.as_object() {
                        object.borrow_mut().props.insert(
                            key,
                            Property {
                                value,
                                get: (!matches!(get, Value::Undefined)).then_some(get),
                                set: (!matches!(set, Value::Undefined)).then_some(set),
                                enumerable: descriptor.get("enumerable").truthy(),
                            },
                        );
                        Some(Value::Bool(true))
                    } else {
                        Some(Value::Bool(false))
                    }
                }
            }
            "Reflect.deleteProperty" => {
                if a0.kind() == "Proxy" {
                    Some(Value::Bool(
                        self.proxy_delete(doc, &a0, &property_key(&a1))?
                            .unwrap_or(false),
                    ))
                } else if let Some(object) = a0.as_object() {
                    let key = property_key(&a1);
                    let mut object = object.borrow_mut();
                    let exists = object.props.contains_key(&key)
                        || key
                            .parse::<usize>()
                            .ok()
                            .is_some_and(|index| index < object.indexed.len());
                    let non_configurable =
                        object.frozen || (key == "length" && object.kind == "Array");
                    if exists && non_configurable {
                        Some(Value::Bool(false))
                    } else {
                        object.props.remove(&key);
                        if let Ok(index) = key.parse::<usize>() {
                            if let Some(value) = object.indexed.get_mut(index) {
                                *value = Value::Undefined;
                            }
                        }
                        Some(Value::Bool(true))
                    }
                } else {
                    return Err(Error::type_error(
                        "Reflect.deleteProperty target must be an object",
                    ));
                }
            }
            "Reflect.getPrototypeOf" => Some(
                a0.as_object()
                    .and_then(|object| object.borrow().prototype.clone())
                    .unwrap_or(Value::Null),
            ),
            "Reflect.setPrototypeOf" => {
                if let Some(object) = a0.as_object() {
                    object.borrow_mut().prototype = Some(a1);
                    Some(Value::Bool(true))
                } else {
                    Some(Value::Bool(false))
                }
            }
            "Reflect.isExtensible" => Some(Value::Bool(
                a0.as_object().is_some_and(|object| !object.borrow().frozen),
            )),
            "Reflect.preventExtensions" => {
                if let Some(object) = a0.as_object() {
                    object.borrow_mut().frozen = true;
                    Some(Value::Bool(true))
                } else {
                    Some(Value::Bool(false))
                }
            }
            "Reflect.apply" => Some(self.call(doc, a0, a1, array_like(&arg(2)), false)?),
            "Reflect.construct" => {
                Some(self.call(doc, a0, Value::Undefined, array_like(&a1), true)?)
            }
            "Symbol" => {
                if construct {
                    return Err(Error::type_error("Symbol is not a constructor"));
                }
                let symbol = Value::object("Symbol");
                let id = NEXT_SYMBOL_ID.fetch_add(1, Ordering::Relaxed).max(1);
                if let Some(object) = symbol.as_object() {
                    object.borrow_mut().payload = Some(Rc::new(format!(
                        "Symbol.unique:{id}:{}",
                        args.first().map(string).unwrap_or_default()
                    )));
                }
                self.register_symbol_key(&symbol);
                Some(symbol)
            }
            "Iterator.create" => {
                let values = if this.kind() == "Map" {
                    map_entries(&this)
                        .into_iter()
                        .map(|(key, value)| Value::array(vec![key, value]))
                        .collect()
                } else if this.kind() == "Set" {
                    this.items()
                } else if matches!(this, Value::String(_)) {
                    this.text()
                        .chars()
                        .map(|character| Value::string(character.to_string()))
                        .collect()
                } else {
                    self.array_values(doc, &this)?
                };
                Some(make_iterator(values))
            }
            "Iterator.next" => {
                let mut state =
                    iterator_state(&this).ok_or_else(|| Error::type_error("invalid iterator"))?;
                let value = state
                    .values
                    .get(state.cursor)
                    .cloned()
                    .unwrap_or(Value::Undefined);
                let done = state.cursor >= state.values.len();
                if !done {
                    state.cursor += 1;
                }
                if let Some(object) = this.as_object() {
                    object.borrow_mut().payload = Some(Rc::new(state));
                }
                let result = Value::object("Object");
                result.put("value", value);
                result.put("done", Value::Bool(done));
                Some(result)
            }
            "Symbol.for" => {
                let key = string(&a0);
                let registry = self.global.get("\0symbol_registry");
                let value = registry.get(&key);
                if value.kind() == "Undefined" {
                    let symbol = Value::object("Symbol");
                    if let Some(object) = symbol.as_object() {
                        object.borrow_mut().payload = Some(Rc::new(format!("Symbol.for:{key}")));
                    }
                    self.register_symbol_key(&symbol);
                    registry.put(&key, symbol.clone());
                    Some(symbol)
                } else {
                    Some(value)
                }
            }
            "Symbol.keyFor" => Some(
                self.global
                    .get("\0symbol_registry")
                    .as_object()
                    .and_then(|registry| {
                        registry.borrow().props.iter().find_map(|(key, property)| {
                            property.value.same(&a0).then(|| Value::string(key.clone()))
                        })
                    })
                    .unwrap_or(Value::Undefined),
            ),
            "Boolean" => Some(Value::Bool(a0.truthy())),
            "isFinite" => Some(Value::Bool(number(&a0).is_finite())),
            "isNaN" => Some(Value::Bool(number(&a0).is_nan())),
            "Number.isFinite" => Some(Value::Bool(matches!(a0, Value::Number(n) if n.is_finite()))),
            "Number.isInteger" | "Number.isSafeInteger" => Some(Value::Bool(
                matches!(a0, Value::Number(n) if n.is_finite() && n.fract() == 0.0 && (name == "Number.isInteger" || n.abs() <= 9_007_199_254_740_991.0)),
            )),
            "Number.isNaN" => Some(Value::Bool(matches!(a0, Value::Number(n) if n.is_nan()))),
            "Number.parseFloat" | "parseFloat" => Some(Value::Number(parse_float(&string(&a0)))),
            "Number.parseInt" | "parseInt" => Some(Value::Number(parse_int(
                &string(&a0),
                if args.len() > 1 {
                    integer(&a1) as u32
                } else {
                    0
                },
            ))),
            "JSON.parse" => {
                let parsed = serde_json::from_str::<serde_json::Value>(&string(&a0))
                    .map_err(|e| Error::syntax(e.to_string()))?;
                Some(Value::from_json(parsed))
            }
            "JSON.stringify" => Some(match json_value(&a0, &mut Vec::new(), 0, false) {
                Ok(v) => Value::string(serde_json::to_string(&v).unwrap_or_else(|_| "null".into())),
                Err(_) => Value::Undefined,
            }),
            "Math.abs" => Some(Value::Number(number(&a0).abs())),
            "Math.ceil" => Some(Value::Number(number(&a0).ceil())),
            "Math.floor" => Some(Value::Number(number(&a0).floor())),
            "Math.round" => Some(Value::Number((number(&a0) + 0.5).floor())),
            "Math.trunc" => Some(Value::Number(number(&a0).trunc())),
            "Math.max" => Some(Value::Number(
                args.iter().map(number).fold(f64::NEG_INFINITY, f64::max),
            )),
            "Math.min" => Some(Value::Number(
                args.iter().map(number).fold(f64::INFINITY, f64::min),
            )),
            "Math.pow" => Some(Value::Number(number(&a0).powf(number(&a1)))),
            "Math.sqrt" => Some(Value::Number(number(&a0).sqrt())),
            "Math.cbrt" => Some(Value::Number(number(&a0).cbrt())),
            "Math.sign" => Some(Value::Number(number(&a0).signum())),
            "Math.log" => Some(Value::Number(number(&a0).ln())),
            "Math.log2" => Some(Value::Number(number(&a0).log2())),
            "Math.log10" => Some(Value::Number(number(&a0).log10())),
            "Math.exp" => Some(Value::Number(number(&a0).exp())),
            "Math.sin" => Some(Value::Number(number(&a0).sin())),
            "Math.cos" => Some(Value::Number(number(&a0).cos())),
            "Math.tan" => Some(Value::Number(number(&a0).tan())),
            "Math.asin" => Some(Value::Number(number(&a0).asin())),
            "Math.acos" => Some(Value::Number(number(&a0).acos())),
            "Math.atan" => Some(Value::Number(number(&a0).atan())),
            "Math.atan2" => Some(Value::Number(number(&a0).atan2(number(&a1)))),
            "Math.hypot" => Some(Value::Number(
                args.iter().map(number).map(|n| n * n).sum::<f64>().sqrt(),
            )),
            "Math.imul" => Some(Value::Number(
                ((integer(&a0) as i32).wrapping_mul(integer(&a1) as i32)) as f64,
            )),
            "Math.clz32" => Some(Value::Number((integer(&a0) as u32).leading_zeros() as f64)),
            "Math.random" => Some(Value::Number(random_unit()?)),
            "Map" => {
                let out = Value::object("Map");
                if !matches!(a0, Value::Undefined | Value::Null) {
                    for pair in self.iterable_to_vec(doc, &a0)? {
                        let items = pair.items();
                        if items.len() < 2 {
                            return Err(Error::type_error(
                                "map entry must contain a key and value",
                            ));
                        }
                        map_set(&out, items[0].clone(), items[1].clone());
                    }
                }
                Some(out)
            }
            "Set" => {
                let out = Value::object("Set");
                if !matches!(a0, Value::Undefined | Value::Null) {
                    for v in self.iterable_to_vec(doc, &a0)? {
                        if !out.items().iter().any(|x| same_value(x, &v)) {
                            if let Some(o) = out.as_object() {
                                o.borrow_mut().indexed.push(v);
                            }
                        }
                    }
                }
                Some(out)
            }
            "Map.groupBy" => {
                if !super::proxy::is_callable(&a1) {
                    return Err(Error::type_error("Map.groupBy callback must be callable"));
                }
                let out = Value::object("Map");
                for (index, value) in self.iterable_to_vec(doc, &a0)?.into_iter().enumerate() {
                    let key = self.call(
                        doc,
                        a1.clone(),
                        Value::Undefined,
                        vec![value.clone(), Value::Number(index as f64)],
                        false,
                    )?;
                    let group = map_get(&out, &key).unwrap_or_else(|| {
                        let group = Value::array(Vec::new());
                        map_set(&out, key.clone(), group.clone());
                        group
                    });
                    if let Some(object) = group.as_object() {
                        object.borrow_mut().indexed.push(value);
                    }
                }
                Some(out)
            }
            "Map.set" => {
                map_set(&this, a0, a1);
                Some(this)
            }
            "Map.get" => Some(map_get(&this, &a0).unwrap_or(Value::Undefined)),
            "Map.has" => Some(Value::Bool(map_get(&this, &a0).is_some())),
            "Map.delete" => Some(Value::Bool(map_delete(&this, &a0))),
            "Map.clear" => {
                if let Some(o) = this.as_object() {
                    o.borrow_mut().indexed.clear();
                }
                Some(Value::Undefined)
            }
            "Map.keys" => Some(make_iterator(
                map_entries(&this).into_iter().map(|(k, _)| k).collect(),
            )),
            "Map.values" => Some(make_iterator(
                map_entries(&this).into_iter().map(|(_, v)| v).collect(),
            )),
            "Map.entries" => Some(make_iterator(
                map_entries(&this)
                    .into_iter()
                    .map(|(k, v)| Value::array(vec![k, v]))
                    .collect(),
            )),
            "Map.forEach" => {
                if !super::proxy::is_callable(&a0) {
                    return Err(Error::type_error("Map.forEach callback must be callable"));
                }
                for (k, v) in map_entries(&this) {
                    self.call(doc, a0.clone(), a1.clone(), vec![v, k, this.clone()], false)?;
                }
                Some(Value::Undefined)
            }
            "Set.add" => {
                if !this.items().iter().any(|x| same_value(x, &a0)) {
                    if let Some(o) = this.as_object() {
                        o.borrow_mut().indexed.push(a0);
                    }
                }
                Some(this)
            }
            "Set.has" => Some(Value::Bool(this.items().iter().any(|x| same_value(x, &a0)))),
            "Set.delete" => Some(Value::Bool(set_delete(&this, &a0))),
            "Set.clear" => {
                if let Some(o) = this.as_object() {
                    o.borrow_mut().indexed.clear();
                }
                Some(Value::Undefined)
            }
            "Set.values" | "Set.keys" => Some(make_iterator(this.items())),
            "Set.forEach" => {
                if !super::proxy::is_callable(&a0) {
                    return Err(Error::type_error("Set.forEach callback must be callable"));
                }
                for value in this.items() {
                    self.call(
                        doc,
                        a0.clone(),
                        a1.clone(),
                        vec![value.clone(), value, this.clone()],
                        false,
                    )?;
                }
                Some(Value::Undefined)
            }
            "Set.entries" => Some(make_iterator(
                this.items()
                    .into_iter()
                    .map(|v| Value::array(vec![v.clone(), v]))
                    .collect(),
            )),
            "ArrayBuffer" => Some(if a0.kind().ends_with("Array") {
                make_buffer_from(&a0)
            } else {
                make_buffer(integer(&a0).clamp(0, MAX_COLLECTION as i64) as usize)
            }),
            "Uint8Array" | "Uint8ClampedArray" | "Uint16Array" | "Uint32Array" | "Int8Array"
            | "Int16Array" | "Int32Array" | "Float32Array" | "Float64Array" => {
                Some(make_typed(name, &a0, &a1, args.get(2)))
            }
            "Error" | "TypeError" | "RangeError" | "ReferenceError" | "SyntaxError" => {
                let error = Value::error(name, args_string(&args, 0, ""));
                if let Some(object) = error.as_object() {
                    object.borrow_mut().prototype = Some(self.global.get(name).get("prototype"));
                }
                if !matches!(a1, Value::Undefined) {
                    let cause = self.get_property(doc, &a1, "cause")?;
                    if !matches!(cause, Value::Undefined) {
                        error.put("cause", cause);
                    }
                }
                Some(error)
            }
            "Function.call" => {
                Some(self.call(doc, this, a0, args.into_iter().skip(1).collect(), false)?)
            }
            "Function.apply" => Some(self.call(doc, this, a0, array_like(&a1), false)?),
            "Function.bind" => Some(bind_function(
                &self.global,
                this,
                a0,
                args.into_iter().skip(1).collect(),
            )),
            bound_name if bound_name.starts_with("Function.bound.") => {
                let id = bound_name
                    .strip_prefix("Function.bound.")
                    .ok_or_else(|| Error::type_error("invalid bound function"))?;
                let bound = bound_call(&self.global.get("\0bound_function_registry").get(id))
                    .ok_or_else(|| Error::type_error("invalid bound function"))?;
                let mut call_args = bound.args;
                call_args.extend(args);
                Some(self.call(doc, bound.target, bound.this, call_args, construct)?)
            }
            "Number.toLocaleString" | "Number.prototype.toLocaleString" => {
                Some(Value::string(format_locale_number(&this, &a0)))
            }
            "Number.prototype.toFixed" => {
                let digits = number_digits(&a0, 0, 0, 100)?;
                Some(Value::string(fixed_number_text(number(&this), digits)))
            }
            "Number.prototype.toExponential" => {
                let value = number(&this);
                let digits = if matches!(a0, Value::Undefined) {
                    None
                } else {
                    Some(number_digits(&a0, 0, 0, 100)?)
                };
                if !value.is_finite() {
                    Some(Value::string(number_text(value)))
                } else if let Some(digits) = digits {
                    Some(Value::string(exponential_text(value, digits)))
                } else {
                    Some(Value::string(shortest_exponential_text(value)))
                }
            }
            "Number.prototype.toPrecision" => {
                let value = number(&this);
                let precision = if matches!(a0, Value::Undefined) {
                    None
                } else {
                    Some(number_digits(&a0, 1, 1, 100)?)
                };
                if precision.is_none() || !value.is_finite() {
                    Some(Value::string(number_text(value)))
                } else {
                    Some(Value::string(precision_number_text(
                        value,
                        precision.unwrap_or(1),
                    )))
                }
            }
            "Number.prototype.toString" => Some(Value::string(string(&this))),
            "Number.prototype.valueOf" => Some(this.clone()),
            "String.prototype.toLocaleString" => Some(Value::string(string(&this))),
            "Function.toString" => Some(Value::string(string(&this))),
            "String.prototype.toString" | "String.prototype.valueOf" => {
                Some(Value::string(string(&this)))
            }
            "Array.toString"
            | "NodeList.toString"
            | "Uint8Array.toString"
            | "Uint8ClampedArray.toString"
            | "Uint16Array.toString"
            | "Uint32Array.toString"
            | "Int8Array.toString"
            | "Int16Array.toString"
            | "Int32Array.toString"
            | "Float32Array.toString"
            | "Float64Array.toString" => Some(Value::string(
                self.array_values(doc, &this)?
                    .iter()
                    .map(|value| {
                        if matches!(value, Value::Null | Value::Undefined) {
                            String::new()
                        } else {
                            string(value)
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(","),
            )),
            "Error.toString"
            | "TypeError.toString"
            | "RangeError.toString"
            | "ReferenceError.toString"
            | "SyntaxError.toString" => Some(Value::string(this.text())),
            "String.fromCharCode" => Some(Value::string(String::from_utf16_lossy(
                &args.iter().map(|v| integer(v) as u16).collect::<Vec<_>>(),
            ))),
            "String.fromCodePoint" => Some(Value::string(
                args.iter()
                    .filter_map(|v| char::from_u32(integer(v) as u32))
                    .collect::<String>(),
            )),
            "String.prototype.concat" => Some(Value::string(
                std::iter::once(string(&this))
                    .chain(args.iter().map(string))
                    .collect::<String>(),
            )),
            "String.prototype.at"
            | "String.prototype.charAt"
            | "String.prototype.charCodeAt"
            | "String.prototype.codePointAt" => {
                let units = string_units(&this);
                let raw = integer(&a0);
                let index = if name.ends_with(".at") && raw < 0 {
                    units.len() as i64 + raw
                } else {
                    raw
                };
                if index < 0 || index as usize >= units.len() {
                    Some(match name {
                        "String.prototype.at" => Value::Undefined,
                        "String.prototype.charAt" => Value::string(""),
                        _ => Value::Number(f64::NAN),
                    })
                } else {
                    let index = index as usize;
                    Some(match name {
                        "String.prototype.at" | "String.prototype.charAt" => {
                            Value::String(vec![units[index]].into())
                        }
                        "String.prototype.charCodeAt" => Value::Number(units[index] as f64),
                        _ => {
                            let unit = units[index];
                            if (0xd800..=0xdbff).contains(&unit)
                                && units
                                    .get(index + 1)
                                    .is_some_and(|next| (0xdc00..=0xdfff).contains(next))
                            {
                                let code = 0x10000
                                    + (((unit as u32 - 0xd800) << 10)
                                        | (units[index + 1] as u32 - 0xdc00));
                                Value::Number(code as f64)
                            } else {
                                Value::Number(unit as f64)
                            }
                        }
                    })
                }
            }
            "String.prototype.includes"
            | "String.prototype.startsWith"
            | "String.prototype.endsWith" => {
                let units = string_units(&this);
                let needle = string_units(&a0);
                let has_position = args.len() > 1 && !matches!(a1, Value::Undefined);
                let position = if has_position {
                    integer(&a1).clamp(0, units.len() as i64) as usize
                } else if name.ends_with("endsWith") {
                    units.len()
                } else {
                    0
                };
                let found = match name {
                    "String.prototype.includes" => {
                        find_units(&units, &needle, position, units.len()).is_some()
                    }
                    "String.prototype.startsWith" => units
                        .get(position..position.saturating_add(needle.len()))
                        .is_some_and(|slice| slice == needle),
                    _ => {
                        let end = if args.len() > 1 {
                            position
                        } else {
                            units.len()
                        };
                        let start = end.saturating_sub(needle.len());
                        units.get(start..end).is_some_and(|slice| slice == needle)
                    }
                };
                Some(Value::Bool(found))
            }
            "String.prototype.indexOf" | "String.prototype.lastIndexOf" => {
                let units = string_units(&this);
                let needle = string_units(&a0);
                let position = if args.len() > 1 && !matches!(a1, Value::Undefined) {
                    integer(&a1).clamp(0, units.len() as i64) as usize
                } else if name.ends_with("lastIndexOf") {
                    units.len()
                } else {
                    0
                };
                let found = if name.ends_with("lastIndexOf") {
                    if needle.is_empty() {
                        Some(position)
                    } else if needle.len() > units.len() {
                        None
                    } else {
                        (0..=position.min(units.len() - needle.len()))
                            .rev()
                            .find(|index| units[*index..*index + needle.len()] == needle)
                    }
                } else {
                    find_units(&units, &needle, position, units.len())
                };
                Some(Value::Number(found.map_or(-1.0, |index| index as f64)))
            }
            "String.prototype.toLowerCase" => Some(Value::string(string(&this).to_lowercase())),
            "String.prototype.toUpperCase" => Some(Value::string(string(&this).to_uppercase())),
            "String.prototype.trim" | "String.prototype.trimStart" | "String.prototype.trimEnd" => {
                Some(Value::string(match name {
                    "String.prototype.trimStart" => string(&this).trim_start().to_owned(),
                    "String.prototype.trimEnd" => string(&this).trim_end().to_owned(),
                    _ => string(&this).trim().to_owned(),
                }))
            }
            "String.prototype.slice" | "String.prototype.substring" => {
                let units = string_units(&this);
                let len = units.len();
                let substring = name.ends_with("substring");
                let start = if substring {
                    integer(&a0).clamp(0, len as i64) as usize
                } else {
                    index(&a0, len)
                };
                let end = if args.len() > 1 {
                    if substring {
                        integer(&a1).clamp(0, len as i64) as usize
                    } else {
                        index(&a1, len)
                    }
                } else {
                    len
                };
                let (lo, hi) = if substring && start > end {
                    (end, start)
                } else {
                    (start, end.max(start))
                };
                Some(Value::String(
                    units[lo.min(len)..hi.min(len)].to_vec().into(),
                ))
            }
            "String.prototype.substr" => {
                let units = string_units(&this);
                let len = units.len();
                let start = index(&a0, len);
                let count = if args.len() > 1 {
                    integer(&a1).max(0) as usize
                } else {
                    len - start
                };
                Some(Value::String(
                    units[start..start.saturating_add(count).min(len)]
                        .to_vec()
                        .into(),
                ))
            }
            "String.prototype.split" => {
                if a0.kind() == "RegExp" {
                    return self.regexp_call(doc, "RegexString.split", this, args);
                }
                let s = string(&this);
                let sep = string(&a0);
                Some(Value::array(if matches!(a0, Value::Undefined) {
                    vec![Value::string(s)]
                } else if sep.is_empty() {
                    array_from_utf16(&s)
                } else {
                    s.split(&sep).map(Value::string).collect()
                }))
            }
            "String.prototype.repeat" => {
                let n = integer(&a0).clamp(0, 100_000) as usize;
                Some(Value::string(string(&this).repeat(n)))
            }
            "String.prototype.padStart" | "String.prototype.padEnd" => {
                let mut units = string_units(&this);
                let target = number(&a0);
                if target == f64::INFINITY || target > MAX_COLLECTION as f64 {
                    return Err(Error::new(
                        "RangeError",
                        "padded string exceeds the length limit",
                    ));
                }
                let target = if target.is_nan() || target <= 0.0 {
                    0
                } else {
                    target.trunc() as usize
                };
                if target <= units.len() {
                    Some(Value::String(units.into()))
                } else {
                    let filler = if args.len() < 2 || matches!(a1, Value::Undefined) {
                        vec![u16::from(b' ')]
                    } else {
                        string_units(&a1)
                    };
                    if filler.is_empty() {
                        Some(Value::String(units.into()))
                    } else {
                        let count = target - units.len();
                        let padding = (0..count)
                            .map(|index| filler[index % filler.len()])
                            .collect::<Vec<_>>();
                        if name.ends_with("padStart") {
                            let mut padded = padding;
                            padded.extend(units);
                            units = padded;
                        } else {
                            units.extend(padding);
                        }
                        Some(Value::String(units.into()))
                    }
                }
            }
            "String.prototype.normalize" => {
                return Err(Error::new(
                    "NotSupportedError",
                    "Unicode normalization is not available",
                ));
            }
            "String.prototype.match"
            | "String.prototype.replace"
            | "String.prototype.replaceAll"
            | "String.prototype.search" => self.regexp_call(
                doc,
                &format!("RegexString.{}", name.rsplit('.').next().unwrap_or(name)),
                this,
                args,
            )?,
            _ if name.starts_with("Array.")
                || name.starts_with("NodeList.")
                || name.contains("Array.") =>
            {
                Some(self.array_method(doc, name, this, args)?)
            }
            _ if name.starts_with("Uint")
                || name.starts_with("Int")
                || name.starts_with("Float") =>
            {
                Some(self.typed_method(doc, name, this, args)?)
            }
            _ => None,
        };
        Ok(result)
    }

    fn array_values(&mut self, doc: &mut BaseDocument, target: &Value) -> Result<Vec<Value>> {
        let length = self.get_property(doc, target, "length")?;
        let length = integer(&length).clamp(0, MAX_COLLECTION as i64) as usize;
        let mut values = Vec::with_capacity(length);
        for index in 0..length {
            values.push(self.get_property(doc, target, &index.to_string())?);
        }
        Ok(values)
    }

    fn replace_array_values(
        &mut self,
        doc: &mut BaseDocument,
        target: &Value,
        values: Vec<Value>,
    ) -> Result<()> {
        if values.len() > MAX_COLLECTION {
            return Err(Error::new("RangeError", "array length exceeds limit"));
        }
        let old_length = integer(&self.get_property(doc, target, "length")?)
            .clamp(0, MAX_COLLECTION as i64) as usize;
        for (index, value) in values.iter().cloned().enumerate() {
            self.set_property(doc, target, &index.to_string(), value)?;
        }
        for index in (values.len()..old_length).rev() {
            let key = index.to_string();
            if target.kind() == "Proxy" {
                if !self.proxy_delete(doc, target, &key)?.unwrap_or(false) {
                    return Err(Error::type_error(
                        "array method could not delete a Proxy index",
                    ));
                }
            } else if let Some(object) = target.as_object() {
                let mut object = object.borrow_mut();
                object.props.remove(&key);
                if let Some(value) = object.indexed.get_mut(index) {
                    *value = Value::Undefined;
                }
            }
        }
        self.set_property(doc, target, "length", Value::Number(values.len() as f64))
    }

    fn array_method(
        &mut self,
        doc: &mut BaseDocument,
        name: &str,
        this: Value,
        args: Vec<Value>,
    ) -> Result<Value> {
        let method = name.rsplit('.').next().unwrap_or(name);
        if matches!(method, "push" | "pop" | "slice" | "at") {
            let length = integer(&self.get_property(doc, &this, "length")?)
                .clamp(0, MAX_COLLECTION as i64) as usize;
            match method {
                "push" => {
                    let new_length = length
                        .checked_add(args.len())
                        .ok_or_else(|| Error::new("RangeError", "array length exceeds limit"))?;
                    if new_length > MAX_COLLECTION {
                        return Err(Error::new("RangeError", "array length exceeds limit"));
                    }
                    for (offset, value) in args.into_iter().enumerate() {
                        self.set_property(doc, &this, &(length + offset).to_string(), value)?;
                    }
                    self.set_property(doc, &this, "length", Value::Number(new_length as f64))?;
                    return Ok(Value::Number(new_length as f64));
                }
                "pop" => {
                    if length == 0 {
                        self.set_property(doc, &this, "length", Value::Number(0.0))?;
                        return Ok(Value::Undefined);
                    }
                    let index = length - 1;
                    let key = index.to_string();
                    let value = self.get_property(doc, &this, &key)?;
                    if this.kind() == "Proxy" {
                        if !self.proxy_delete(doc, &this, &key)?.unwrap_or(false) {
                            return Err(Error::type_error(
                                "array pop could not delete a Proxy index",
                            ));
                        }
                    } else if let Some(object) = this.as_object() {
                        let mut object = object.borrow_mut();
                        if object.frozen {
                            return Err(Error::type_error("array pop could not delete an index"));
                        }
                        object.props.remove(&key);
                        if let Some(slot) = object.indexed.get_mut(index) {
                            *slot = Value::Undefined;
                        }
                    } else {
                        return Err(Error::type_error("array pop receiver must be an object"));
                    }
                    self.set_property(doc, &this, "length", Value::Number(index as f64))?;
                    return Ok(value);
                }
                "slice" => {
                    let start_arg = args.first().cloned().unwrap_or(Value::Undefined);
                    let end_arg = args.get(1).cloned().unwrap_or(Value::Undefined);
                    let start = index(&start_arg, length).min(length);
                    let end = if args.len() > 1 {
                        index(&end_arg, length).min(length)
                    } else {
                        length
                    };
                    let mut values = Vec::with_capacity(end.saturating_sub(start));
                    for index in start..end.max(start) {
                        values.push(self.get_property(doc, &this, &index.to_string())?);
                    }
                    return Ok(Value::array(values));
                }
                "at" => {
                    let offset = integer(&args.first().cloned().unwrap_or(Value::Undefined));
                    let index = if offset < 0 {
                        (length as i64).saturating_add(offset)
                    } else {
                        offset
                    };
                    if index < 0 || index as usize >= length {
                        return Ok(Value::Undefined);
                    }
                    return self.get_property(doc, &this, &index.to_string());
                }
                _ => unreachable!(),
            }
        }
        let mut xs = self.array_values(doc, &this)?;
        let a0 = args.first().cloned().unwrap_or(Value::Undefined);
        let a1 = args.get(1).cloned().unwrap_or(Value::Undefined);
        let callback_this = args.get(1).cloned().unwrap_or(Value::Undefined);
        let out = match name.rsplit('.').next().unwrap_or(name) {
            "shift" => {
                let v = if xs.is_empty() {
                    Value::Undefined
                } else {
                    xs.remove(0)
                };
                self.replace_array_values(doc, &this, xs)?;
                v
            }
            "unshift" => {
                let n = xs.len() + args.len();
                xs.splice(0..0, args);
                self.replace_array_values(doc, &this, xs)?;
                Value::Number(n as f64)
            }
            "reverse" => {
                xs.reverse();
                self.replace_array_values(doc, &this, xs)?;
                this.clone()
            }
            "concat" => {
                let mut out = xs;
                for v in args {
                    if super::proxy::is_array(&v)? {
                        out.extend(self.array_values(doc, &v)?);
                    } else {
                        out.push(v)
                    }
                }
                Value::array(out)
            }
            "join" => Value::string(
                xs.iter()
                    .map(|v| {
                        if matches!(v, Value::Null | Value::Undefined) {
                            String::new()
                        } else {
                            string(v)
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(&args_string(&args, 0, ",")),
            ),
            "includes" | "indexOf" | "lastIndexOf" => {
                let start = if args.len() > 1 {
                    index(&a1, xs.len())
                } else {
                    0
                };
                let found = xs
                    .iter()
                    .enumerate()
                    .skip(start)
                    .find(|(_, v)| {
                        if name.ends_with("includes") {
                            same_value_zero(v, &a0)
                        } else {
                            same_value(v, &a0)
                        }
                    })
                    .map(|(i, _)| i);
                if name.ends_with("includes") {
                    Value::Bool(found.is_some())
                } else {
                    Value::Number(found.map(|i| i as f64).unwrap_or(-1.0))
                }
            }
            "map" | "filter" | "forEach" | "some" | "every" | "find" | "findIndex" => {
                let mut mapped = Vec::new();
                let mut answer = Value::Undefined;
                let mut truth = name.ends_with("every");
                for (i, v) in xs.iter().cloned().enumerate() {
                    let r = self.call(
                        doc,
                        a0.clone(),
                        callback_this.clone(),
                        vec![v.clone(), Value::Number(i as f64), this.clone()],
                        false,
                    )?;
                    let yes = r.truthy();
                    match name.rsplit('.').next().unwrap() {
                        "map" => mapped.push(r),
                        "filter" => {
                            if yes {
                                mapped.push(v)
                            }
                        }
                        "some" => {
                            if yes {
                                truth = true;
                                break;
                            }
                        }
                        "every" => {
                            if !yes {
                                truth = false;
                                break;
                            }
                        }
                        "find" => {
                            if yes {
                                answer = v;
                                break;
                            }
                        }
                        "findIndex" if yes => {
                            answer = Value::Number(i as f64);
                            break;
                        }
                        _ => {}
                    }
                }
                match name.rsplit('.').next().unwrap() {
                    "map" | "filter" => Value::array(mapped),
                    "forEach" => Value::Undefined,
                    "some" | "every" => Value::Bool(truth),
                    _ => answer,
                }
            }
            "reduce" | "reduceRight" => {
                let mut i = if name.ends_with("reduceRight") {
                    xs.len() as isize - 1
                } else {
                    0
                };
                let step = if name.ends_with("reduceRight") { -1 } else { 1 };
                let mut acc = if args.len() > 1 {
                    a1
                } else if xs.is_empty() {
                    return Err(Error::type_error(
                        "reduce of empty array with no initial value",
                    ));
                } else {
                    let v = xs[i as usize].clone();
                    i += step;
                    v
                };
                while i >= 0 && (i as usize) < xs.len() {
                    acc = self.call(
                        doc,
                        a0.clone(),
                        Value::Undefined,
                        vec![
                            acc,
                            xs[i as usize].clone(),
                            Value::Number(i as f64),
                            this.clone(),
                        ],
                        false,
                    )?;
                    i += step
                }
                acc
            }
            "fill" => {
                let start = if args.len() > 1 {
                    index(&a1, xs.len())
                } else {
                    0
                };
                let end = if args.len() > 2 {
                    index(&args[2], xs.len())
                } else {
                    xs.len()
                };
                for v in xs.iter_mut().take(end).skip(start) {
                    *v = a0.clone()
                }
                self.replace_array_values(doc, &this, xs)?;
                this.clone()
            }
            "copyWithin" => {
                let target = index(&a0, xs.len());
                let start = index(&a1, xs.len());
                let end = if args.len() > 2 {
                    index(&args[2], xs.len())
                } else {
                    xs.len()
                };
                let count = end
                    .saturating_sub(start)
                    .min(xs.len().saturating_sub(target));
                let copied = xs[start..start + count].to_vec();
                xs[target..target + count].clone_from_slice(&copied);
                self.replace_array_values(doc, &this, xs)?;
                this.clone()
            }
            "splice" => {
                let start = index(&a0, xs.len());
                let delete = if args.len() > 1 {
                    integer(&a1).clamp(0, (xs.len() - start) as i64) as usize
                } else {
                    xs.len() - start
                };
                let inserted = args.into_iter().skip(2).collect::<Vec<_>>();
                let removed: Vec<_> = xs.splice(start..start + delete, inserted).collect();
                self.replace_array_values(doc, &this, xs)?;
                Value::array(removed)
            }
            "sort" | "toSorted" => {
                xs.sort_by_key(string);
                if name.ends_with("toSorted") {
                    Value::array(xs)
                } else {
                    self.replace_array_values(doc, &this, xs)?;
                    this.clone()
                }
            }
            "toReversed" => {
                xs.reverse();
                Value::array(xs)
            }
            "toSpliced" => {
                let start = index(&a0, xs.len());
                let delete = if args.len() > 1 {
                    integer(&a1).max(0) as usize
                } else {
                    0
                };
                xs.splice(
                    start..(start + delete).min(xs.len()),
                    args.into_iter().skip(2),
                );
                Value::array(xs)
            }
            "with" => {
                let i = integer(&a0);
                if i < 0 || i as usize >= xs.len() {
                    return Err(Error::new("RangeError", "array index out of range"));
                }
                xs[i as usize] = a1;
                Value::array(xs)
            }
            "flat" => Value::array(flatten(
                xs,
                if args.is_empty() {
                    1
                } else {
                    integer(&a0).max(0) as usize
                },
            )),
            "flatMap" => {
                let mut out = Vec::new();
                for (i, v) in xs.into_iter().enumerate() {
                    let mapped = self.call(
                        doc,
                        a0.clone(),
                        callback_this.clone(),
                        vec![v, Value::Number(i as f64), this.clone()],
                        false,
                    )?;
                    if super::proxy::is_array(&mapped)? {
                        out.extend(self.array_values(doc, &mapped)?);
                    } else {
                        out.push(mapped)
                    }
                }
                Value::array(out)
            }
            "entries" => make_iterator(
                xs.into_iter()
                    .enumerate()
                    .map(|(i, v)| Value::array(vec![Value::Number(i as f64), v]))
                    .collect(),
            ),
            "keys" => make_iterator((0..xs.len()).map(|i| Value::Number(i as f64)).collect()),
            "values" => make_iterator(xs),
            _ => return Err(Error::type_error("unsupported array method")),
        };
        Ok(out)
    }

    fn typed_method(
        &mut self,
        doc: &mut BaseDocument,
        name: &str,
        this: Value,
        args: Vec<Value>,
    ) -> Result<Value> {
        let method = name.rsplit('.').next().unwrap_or(name);
        if method == "set" {
            let offset = integer(args.first().unwrap_or(&Value::Undefined)).max(0) as usize;
            let source = args.get(1).cloned().unwrap_or(Value::Undefined);
            let values = self.iterable_to_vec(doc, &source)?;
            let mut items = this.items();
            if offset.saturating_add(values.len()) > items.len() {
                return Err(Error::new(
                    "RangeError",
                    "typed array source exceeds target",
                ));
            }
            for (index, value) in values.iter().enumerate() {
                items[offset + index] = coerce_element(&this.kind(), value);
            }
            replace_items(&this, items);
            return Ok(Value::Undefined);
        }
        if method == "subarray" {
            let items = this.items();
            let start = index(args.first().unwrap_or(&Value::Number(0.0)), items.len());
            let end = args
                .get(1)
                .map_or(items.len(), |value| index(value, items.len()));
            return Ok(make_typed(
                &this.kind(),
                &Value::array(
                    items[start.min(items.len())..end.max(start).min(items.len())].to_vec(),
                ),
                &Value::Number(0.0),
                None,
            ));
        }
        let native_name = this.kind();
        let arrname = format!("{native_name}.{}", name.rsplit('.').next().unwrap_or(name));
        let result = self.array_method(doc, &arrname, this.clone(), args)?;
        if result.kind() == "Array"
            && [
                "filter",
                "slice",
                "map",
                "toReversed",
                "toSorted",
                "toSpliced",
                "with",
            ]
            .iter()
            .any(|method| *method == name.rsplit('.').next().unwrap_or(name))
        {
            Ok(make_typed(&native_name, &result, &Value::Number(0.0), None))
        } else if result.kind() == "Array"
            && !["entries", "keys", "values"]
                .iter()
                .any(|method| name.ends_with(method))
        {
            replace_items(
                &this,
                result
                    .items()
                    .iter()
                    .map(|value| coerce_element(&native_name, value))
                    .collect(),
            );
            Ok(this)
        } else {
            Ok(result)
        }
    }
}

fn same_value(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => {
            (x.is_nan() && y.is_nan()) || x.to_bits() == y.to_bits() || (*x == 0.0 && *y == 0.0)
        }
        (Value::Object(_), Value::Object(_)) => a.same(b),
        _ => a.same(b),
    }
}
fn same_value_zero(a: &Value, b: &Value) -> bool {
    same_value(a, b)
}
fn replace_items(value: &Value, items: Vec<Value>) {
    if let Some(o) = value.as_object() {
        o.borrow_mut().indexed = items;
    }
}
fn flatten(xs: Vec<Value>, depth: usize) -> Vec<Value> {
    let mut out = Vec::new();
    for x in xs {
        if depth > 0 && x.kind() == "Array" {
            out.extend(flatten(x.items(), depth - 1))
        } else {
            out.push(x)
        }
    }
    out
}
fn map_entries(map: &Value) -> Vec<(Value, Value)> {
    let xs = map.items();
    xs.as_chunks::<2>()
        .0
        .iter()
        .map(|p| (p[0].clone(), p[1].clone()))
        .collect()
}
fn map_get(map: &Value, key: &Value) -> Option<Value> {
    map_entries(map)
        .into_iter()
        .find(|(k, _)| same_value_zero(k, key))
        .map(|(_, v)| v)
}
fn map_set(map: &Value, key: Value, value: Value) {
    if let Some(o) = map.as_object() {
        let mut b = o.borrow_mut();
        let n = b
            .indexed
            .as_chunks::<2>()
            .0
            .iter()
            .position(|p| same_value_zero(&p[0], &key));
        if let Some(i) = n {
            b.indexed[i * 2 + 1] = value
        } else {
            b.indexed.push(key);
            b.indexed.push(value)
        }
    }
}
fn map_delete(map: &Value, key: &Value) -> bool {
    if let Some(o) = map.as_object() {
        let mut b = o.borrow_mut();
        if let Some(i) = b
            .indexed
            .as_chunks::<2>()
            .0
            .iter()
            .position(|p| same_value_zero(&p[0], key))
        {
            b.indexed.drain(i * 2..i * 2 + 2);
            return true;
        }
    }
    false
}
fn set_delete(set: &Value, key: &Value) -> bool {
    if let Some(o) = set.as_object() {
        let mut b = o.borrow_mut();
        if let Some(i) = b.indexed.iter().position(|v| same_value_zero(v, key)) {
            b.indexed.remove(i);
            return true;
        }
    }
    false
}
fn element_bytes(kind: &str, value: &Value) -> Vec<u8> {
    let n = number(value);
    match kind {
        "Uint8Array" | "Uint8ClampedArray" => {
            vec![coerce_integer(n, 8, false, kind == "Uint8ClampedArray") as u8]
        }
        "Int8Array" => vec![coerce_integer(n, 8, true, false) as i8 as u8],
        "Uint16Array" => (coerce_integer(n, 16, false, false) as u16)
            .to_le_bytes()
            .to_vec(),
        "Int16Array" => (coerce_integer(n, 16, true, false) as i16)
            .to_le_bytes()
            .to_vec(),
        "Uint32Array" => (coerce_integer(n, 32, false, false) as u32)
            .to_le_bytes()
            .to_vec(),
        "Int32Array" => (coerce_integer(n, 32, true, false) as i32)
            .to_le_bytes()
            .to_vec(),
        "Float32Array" => (n as f32).to_le_bytes().to_vec(),
        "Float64Array" => n.to_le_bytes().to_vec(),
        _ => Vec::new(),
    }
}
fn coerce_integer(n: f64, bits: u32, signed: bool, clamped: bool) -> i64 {
    if clamped {
        return n.round().clamp(0.0, 255.0) as i64;
    }
    if !n.is_finite() || n == 0.0 {
        return 0;
    }
    let modulus = 2f64.powi(bits as i32);
    let mut value = n.trunc() % modulus;
    if value < 0.0 {
        value += modulus
    }
    if signed && value >= modulus / 2.0 {
        value -= modulus
    }
    value as i64
}
fn bytes_for(value: &Value) -> Vec<u8> {
    if let Some(object) = value.as_object() {
        if let Some(bytes) = object
            .borrow()
            .payload
            .as_ref()
            .and_then(|payload| payload.downcast_ref::<Vec<u8>>())
        {
            return bytes.clone();
        }
    }
    if value.kind() == "ArrayBuffer" {
        return value
            .items()
            .iter()
            .map(|byte| number(byte).clamp(0.0, 255.0) as u8)
            .collect();
    }
    let kind = value.kind();
    value
        .items()
        .iter()
        .flat_map(|item| element_bytes(&kind, item))
        .collect()
}
fn make_buffer(size: usize) -> Value {
    let out = Value::object("ArrayBuffer");
    if let Some(o) = out.as_object() {
        o.borrow_mut().indexed = vec![Value::Number(0.0); size];
    }
    out.put("byteLength", Value::Number(size as f64));
    out
}
fn make_buffer_from(source: &Value) -> Value {
    let bytes = bytes_for(source);
    let out = Value::object("ArrayBuffer");
    if let Some(object) = out.as_object() {
        object.borrow_mut().indexed = bytes
            .iter()
            .map(|byte| Value::Number(*byte as f64))
            .collect();
    }
    out.put("byteLength", Value::Number(bytes.len() as f64));
    out
}
fn make_typed(kind: &str, source: &Value, offset: &Value, length: Option<&Value>) -> Value {
    let width = element_bytes(kind, &Value::Number(0.0)).len().max(1);
    let xs = if source.kind() == "ArrayBuffer" {
        let bytes = bytes_for(source);
        let start = (integer(offset).max(0) as usize).min(bytes.len());
        let available = (bytes.len() - start) / width;
        let count = length
            .map_or(available, |v| integer(v).max(0) as usize)
            .min(available)
            .min(MAX_COLLECTION);
        (0..count)
            .map(|index| {
                decode_element(
                    kind,
                    &bytes[start + index * width..start + (index + 1) * width],
                )
            })
            .collect()
    } else {
        match source {
            Value::Number(n) => {
                vec![Value::Number(0.0); n.trunc().clamp(0.0, MAX_COLLECTION as f64) as usize]
            }
            _ => array_like(source)
                .into_iter()
                .take(MAX_COLLECTION)
                .map(|value| coerce_element(kind, &value))
                .collect(),
        }
    };
    let out = Value::object(kind);
    replace_items(&out, xs);
    out.put(
        "byteLength",
        Value::Number(out.items().len() as f64 * width as f64),
    );
    out.put("byteOffset", Value::Number(0.0));
    out.put("buffer", make_buffer_from(&out));
    out
}
fn coerce_element(kind: &str, value: &Value) -> Value {
    let n = number(value);
    Value::Number(match kind {
        "Float32Array" => (n as f32) as f64,
        "Float64Array" => n,
        "Uint8Array" | "Uint8ClampedArray" => {
            coerce_integer(n, 8, false, kind == "Uint8ClampedArray") as f64
        }
        "Int8Array" => coerce_integer(n, 8, true, false) as f64,
        "Uint16Array" => coerce_integer(n, 16, false, false) as f64,
        "Int16Array" => coerce_integer(n, 16, true, false) as f64,
        "Uint32Array" => coerce_integer(n, 32, false, false) as f64,
        "Int32Array" => coerce_integer(n, 32, true, false) as f64,
        _ => n,
    })
}
fn decode_element(kind: &str, bytes: &[u8]) -> Value {
    let mut raw = [0u8; 8];
    raw[..bytes.len().min(8)].copy_from_slice(&bytes[..bytes.len().min(8)]);
    Value::Number(match kind {
        "Uint8Array" | "Uint8ClampedArray" => raw[0] as f64,
        "Int8Array" => raw[0] as i8 as f64,
        "Uint16Array" => u16::from_le_bytes([raw[0], raw[1]]) as f64,
        "Int16Array" => i16::from_le_bytes([raw[0], raw[1]]) as f64,
        "Uint32Array" => u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]) as f64,
        "Int32Array" => i32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]) as f64,
        "Float32Array" => f32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]) as f64,
        "Float64Array" => f64::from_le_bytes(raw),
        _ => 0.0,
    })
}
fn parse_float(s: &str) -> f64 {
    let trimmed = s.trim_start();
    for i in (1..=trimmed.len()).rev() {
        if trimmed.is_char_boundary(i) {
            if let Ok(n) = trimmed[..i].parse() {
                return n;
            }
        }
    }
    f64::NAN
}
fn parse_int(s: &str, radix: u32) -> f64 {
    let text = s.trim_start();
    let neg = text.starts_with('-');
    let body = text.trim_start_matches(['+', '-']);
    let (digits, base) = if radix == 0 {
        if let Some(x) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
            (x, 16)
        } else {
            (body, 10)
        }
    } else {
        (body, radix)
    };
    if !(2..=36).contains(&base) {
        return f64::NAN;
    }
    let valid: String = digits.chars().take_while(|c| c.is_digit(base)).collect();
    if valid.is_empty() {
        f64::NAN
    } else {
        i64::from_str_radix(&valid, base)
            .map(|n| if neg { -(n as f64) } else { n as f64 })
            .unwrap_or(f64::NAN)
    }
}
fn random_unit() -> Result<f64> {
    use std::io::Read;
    let mut bytes = [0u8; 8];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(|error| {
            Error::new(
                "NotSupportedError",
                format!("secure random source unavailable: {error}"),
            )
        })?;
    let bits = u64::from_ne_bytes(bytes) >> 11;
    Ok(bits as f64 / (1u64 << 53) as f64)
}
fn bind_function(global: &Value, target: Value, this: Value, bound: Vec<Value>) -> Value {
    let id = NEXT_BOUND_ID.fetch_add(1, Ordering::Relaxed).max(1);
    let record = Value::object("BoundFunctionRecord");
    if let Some(object) = record.as_object() {
        object.borrow_mut().payload = Some(Rc::new(BoundCall {
            target,
            this,
            args: bound,
        }));
    }
    global
        .get("\0bound_function_registry")
        .put(&id.to_string(), record);

    Value::native(format!("Function.bound.{id}"))
}

#[derive(Default)]
struct WeakEntries(Vec<(std::rc::Weak<std::cell::RefCell<Object>>, Value)>);
impl<B: RuntimeBridge> Guest<B> {
    pub(super) fn weak_get(&mut self, value: &Value, key: &str) -> Option<Value> {
        let kind = value.kind();
        if (kind == "WeakMap" && matches!(key, "get" | "set" | "has" | "delete"))
            || (kind == "WeakSet" && matches!(key, "add" | "has" | "delete"))
        {
            Some(self.cached_native(&format!("{kind}.{key}")))
        } else {
            None
        }
    }
    pub(super) fn weak_call(
        &mut self,
        name: &str,
        this: Value,
        args: Vec<Value>,
    ) -> Result<Option<Value>> {
        if matches!(name, "WeakMap" | "WeakSet") {
            let collection = Value::object(name);
            if let Some(object) = collection.as_object() {
                object.borrow_mut().payload =
                    Some(Rc::new(std::cell::RefCell::new(WeakEntries::default())));
            }
            if let Some(initial) = args
                .first()
                .filter(|value| !matches!(value, Value::Undefined | Value::Null))
            {
                for entry in initial.items() {
                    let values = if name == "WeakMap" {
                        entry.items()
                    } else {
                        vec![entry, Value::Bool(true)]
                    };
                    self.weak_call(
                        &format!("{name}.{}", if name == "WeakMap" { "set" } else { "add" }),
                        collection.clone(),
                        values,
                    )?;
                }
            }
            return Ok(Some(collection));
        }
        if !name.starts_with("WeakMap.") && !name.starts_with("WeakSet.") {
            return Ok(None);
        }
        let object = this
            .as_object()
            .ok_or_else(|| Error::type_error("invalid weak collection receiver"))?;
        let payload = object
            .borrow()
            .payload
            .clone()
            .ok_or_else(|| Error::type_error("invalid weak collection state"))?;
        let entries = payload
            .downcast_ref::<std::cell::RefCell<WeakEntries>>()
            .ok_or_else(|| Error::type_error("invalid weak collection state"))?;
        let key = args.first().cloned().unwrap_or(Value::Undefined);
        let operation = name.rsplit('.').next().unwrap_or("");
        let Some(key) = key.as_object() else {
            return if matches!(operation, "set" | "add") {
                Err(Error::type_error("weak collection key must be an object"))
            } else {
                Ok(Some(if operation == "get" {
                    Value::Undefined
                } else {
                    Value::Bool(false)
                }))
            };
        };
        let mut entries = entries.borrow_mut();
        entries.0.retain(|(key, _)| key.strong_count() > 0);
        let found = entries.0.iter().position(|(candidate, _)| {
            candidate
                .upgrade()
                .is_some_and(|candidate| Rc::ptr_eq(&candidate, &key))
        });
        let result = match operation {
            "has" => Value::Bool(found.is_some()),
            "get" => found.map_or(Value::Undefined, |index| entries.0[index].1.clone()),
            "delete" => Value::Bool(if let Some(index) = found {
                entries.0.remove(index);
                true
            } else {
                false
            }),
            "set" | "add" => {
                let value = if operation == "add" {
                    Value::Bool(true)
                } else {
                    args.get(1).cloned().unwrap_or(Value::Undefined)
                };
                if let Some(index) = found {
                    entries.0[index].1 = value;
                } else {
                    if entries.0.len() >= MAX_COLLECTION {
                        return Err(Error::new(
                            "QuotaExceededError",
                            "weak collection entry limit exceeded",
                        ));
                    }
                    entries.0.push((Rc::downgrade(&key), value));
                }
                this
            }
            _ => return Err(Error::type_error("unknown weak collection operation")),
        };
        Ok(Some(result))
    }
}

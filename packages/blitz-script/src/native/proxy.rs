use super::*;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_REVOKER_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone)]
pub(super) struct ProxyState {
    target: Value,
    handler: Value,
    revoked: bool,
}

pub(super) fn create(target: Value, handler: Value) -> Result<Value> {
    if target.as_object().is_none() || handler.as_object().is_none() {
        return Err(Error::type_error(
            "Proxy target and handler must be objects",
        ));
    }
    let proxy = Value::object("Proxy");
    if let Some(object) = proxy.as_object() {
        object.borrow_mut().payload = Some(Rc::new(RefCell::new(ProxyState {
            target,
            handler,
            revoked: false,
        })));
    }
    Ok(proxy)
}

fn read_state(value: &Value) -> Result<(Value, Value)> {
    let object = value
        .as_object()
        .ok_or_else(|| Error::type_error("not a Proxy"))?;
    let payload = object
        .borrow()
        .payload
        .clone()
        .ok_or_else(|| Error::type_error("proxy state is unavailable"))?;
    let proxy = payload
        .downcast_ref::<RefCell<ProxyState>>()
        .ok_or_else(|| Error::type_error("proxy state is unavailable"))?;
    let proxy = proxy.borrow();
    if proxy.revoked {
        return Err(Error::type_error("operation on a revoked Proxy"));
    }
    Ok((proxy.target.clone(), proxy.handler.clone()))
}

pub(super) fn is_callable(value: &Value) -> bool {
    match value.kind().as_str() {
        "Function" | "Native" => true,
        "Proxy" => value
            .as_object()
            .and_then(|object| object.borrow().payload.clone())
            .and_then(|payload| {
                payload
                    .downcast_ref::<RefCell<ProxyState>>()
                    .map(|state| state.borrow().target.clone())
            })
            .is_some_and(|target| is_callable(&target)),
        _ => false,
    }
}

pub(super) fn is_array(value: &Value) -> Result<bool> {
    match value.kind().as_str() {
        "Array" => Ok(true),
        "Proxy" => {
            let (target, _) = read_state(value)?;
            is_array(&target)
        }
        _ => Ok(false),
    }
}

pub(super) fn revoke(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    let Some(payload) = object.borrow().payload.clone() else {
        return false;
    };
    let Some(proxy) = payload.downcast_ref::<RefCell<ProxyState>>() else {
        return false;
    };
    proxy.borrow_mut().revoked = true;
    true
}

pub(super) fn revoker(global: &Value, proxy: Value) -> Value {
    let id = NEXT_REVOKER_ID.fetch_add(1, Ordering::Relaxed).max(1);
    global.get("\0proxy_revokers").put(&id.to_string(), proxy);
    Value::native(format!("Proxy.revoke.{id}"))
}

fn trap<B: RuntimeBridge>(
    guest: &mut Guest<B>,
    document: &mut BaseDocument,
    handler: &Value,
    name: &str,
) -> Result<Value> {
    guest.get_property(document, handler, name)
}

fn call_trap<B: RuntimeBridge>(
    guest: &mut Guest<B>,
    document: &mut BaseDocument,
    handler: &Value,
    name: &str,
    args: Vec<Value>,
) -> Result<Option<Value>> {
    let method = trap(guest, document, handler, name)?;
    if matches!(method, Value::Undefined | Value::Null) {
        return Ok(None);
    }
    if !matches!(method.kind().as_str(), "Function" | "Native") {
        return Err(Error::type_error(format!(
            "Proxy {name} trap is not callable"
        )));
    }
    guest
        .call(document, method, handler.clone(), args, false)
        .map(Some)
}

impl<B: RuntimeBridge> Guest<B> {
    pub(super) fn get_property_with_receiver(
        &mut self,
        document: &mut BaseDocument,
        target: &Value,
        key: &str,
        receiver: Value,
    ) -> Result<Value> {
        if target.kind() == "Proxy" {
            return Ok(self
                .proxy_get_with_receiver(document, target, key, receiver)?
                .unwrap_or(Value::Undefined));
        }
        if let Some(object) = target.as_object() {
            let (own, mut prototype) = {
                let object = object.borrow();
                (object.props.get(key).cloned(), object.prototype.clone())
            };
            if let Some(property) = own {
                if let Some(getter) = property.get {
                    return self.call(document, getter, receiver, Vec::new(), false);
                }
                return Ok(property.value);
            }
            for _ in 0..256 {
                let Some(value) = prototype else {
                    break;
                };
                let Some(object) = value.as_object() else {
                    break;
                };
                let (property, next) = {
                    let object = object.borrow();
                    (object.props.get(key).cloned(), object.prototype.clone())
                };
                if let Some(property) = property {
                    if let Some(getter) = property.get {
                        return self.call(document, getter, receiver, Vec::new(), false);
                    }
                    return Ok(property.value);
                }
                prototype = next;
            }
        }
        self.get_property(document, target, key)
    }

    pub(super) fn has_property(
        &mut self,
        document: &mut BaseDocument,
        target: &Value,
        key: &str,
    ) -> Result<bool> {
        if target.as_object().is_none() {
            return Err(Error::type_error("right operand of in is not an object"));
        }
        if target.kind() == "Proxy" {
            return Ok(self.proxy_has(document, target, key)?.unwrap_or(false));
        }
        let mut prototype = Some(target.clone());
        for _ in 0..256 {
            let Some(ref value) = prototype else {
                break;
            };
            let Some(object) = value.as_object() else {
                break;
            };
            let (found, next) = {
                let object = object.borrow();
                (
                    object.props.contains_key(key)
                        || key
                            .parse::<usize>()
                            .ok()
                            .is_some_and(|index| index < object.indexed.len())
                        || (key == "length"
                            && matches!(object.kind.as_str(), "Array" | "NodeList" | "String")),
                    object.prototype.clone(),
                )
            };
            if found {
                return Ok(true);
            }
            prototype = next;
        }
        if prototype.is_some() {
            return Err(Error::new("RangeError", "prototype chain limit exceeded"));
        }
        if self.builtin_get(document, target, key)?.is_some() {
            return Ok(true);
        }
        let value = self.get_property(document, target, key)?;
        Ok(!matches!(value, Value::Undefined))
    }

    pub(super) fn own_property_names(
        &mut self,
        document: &mut BaseDocument,
        target: &Value,
    ) -> Result<Vec<String>> {
        if target.kind() == "Proxy" {
            return Ok(self.proxy_own_keys(document, target)?.unwrap_or_default());
        }
        let Some(object) = target.as_object() else {
            return Err(Error::type_error("ownKeys target must be an object"));
        };
        let object = object.borrow();
        let mut keys: Vec<String> = object.props.keys().cloned().collect();
        if matches!(
            object.kind.as_str(),
            "Array" | "Arguments" | "NodeList" | "HTMLCollection"
        ) || object.kind.ends_with("Array")
        {
            keys.extend((0..object.indexed.len()).map(|index| index.to_string()));
            if matches!(
                object.kind.as_str(),
                "Array" | "Arguments" | "NodeList" | "HTMLCollection"
            ) || object.kind.ends_with("Array")
            {
                keys.push("length".into());
            }
        }
        Ok(keys)
    }

    pub(super) fn ordinary_descriptor(
        &mut self,
        document: &mut BaseDocument,
        target: &Value,
        key: &str,
    ) -> Result<Value> {
        if target.kind() == "Proxy" {
            return Ok(self
                .proxy_descriptor(document, target, key)?
                .unwrap_or(Value::Undefined));
        }
        let Some(object) = target.as_object() else {
            return Err(Error::type_error("descriptor target must be an object"));
        };
        let object = object.borrow();
        if let Some(property) = object.props.get(key) {
            let descriptor = Value::object("Object");
            if let Some(get) = &property.get {
                descriptor.put("get", get.clone());
            }
            if let Some(set) = &property.set {
                descriptor.put("set", set.clone());
            }
            if property.get.is_none() && property.set.is_none() {
                descriptor.put("value", property.value.clone());
                descriptor.put("writable", Value::Bool(!object.frozen));
            }
            descriptor.put("enumerable", Value::Bool(property.enumerable));
            descriptor.put("configurable", Value::Bool(!object.frozen));
            return Ok(descriptor);
        }
        if let Ok(index) = key.parse::<usize>() {
            if let Some(value) = object.indexed.get(index) {
                let descriptor = Value::object("Object");
                descriptor.put("value", value.clone());
                descriptor.put("writable", Value::Bool(!object.frozen));
                descriptor.put("enumerable", Value::Bool(true));
                descriptor.put("configurable", Value::Bool(!object.frozen));
                return Ok(descriptor);
            }
        }
        if key == "length"
            && (matches!(
                object.kind.as_str(),
                "Array" | "Arguments" | "NodeList" | "HTMLCollection"
            ) || object.kind.ends_with("Array"))
        {
            let descriptor = Value::object("Object");
            descriptor.put("value", Value::Number(object.indexed.len() as f64));
            descriptor.put("writable", Value::Bool(!object.frozen));
            descriptor.put("enumerable", Value::Bool(false));
            descriptor.put("configurable", Value::Bool(false));
            return Ok(descriptor);
        }
        Ok(Value::Undefined)
    }

    pub(super) fn proxy_get(
        &mut self,
        document: &mut BaseDocument,
        proxy: &Value,
        key: &str,
    ) -> Result<Option<Value>> {
        self.proxy_get_with_receiver(document, proxy, key, proxy.clone())
    }

    pub(super) fn proxy_get_with_receiver(
        &mut self,
        document: &mut BaseDocument,
        proxy: &Value,
        key: &str,
        receiver: Value,
    ) -> Result<Option<Value>> {
        if proxy.kind() != "Proxy" {
            return Ok(None);
        }
        let (target, handler) = read_state(proxy)?;
        let fallback_receiver = receiver.clone();
        let result = call_trap(
            self,
            document,
            &handler,
            "get",
            vec![
                target.clone(),
                super::exec::property_key_value(&self.global, key),
                receiver,
            ],
        )?;
        if let Some(value) = result {
            let descriptor = self.ordinary_descriptor(document, &target, key)?;
            if !matches!(&descriptor, Value::Undefined)
                && !descriptor.get("configurable").truthy()
                && !descriptor.get("writable").truthy()
                && !value.same(&descriptor.get("value"))
            {
                return Err(Error::type_error(
                    "Proxy get trap changed a frozen data property",
                ));
            }
            Ok(Some(value))
        } else {
            Ok(Some(self.get_property_with_receiver(
                document,
                &target,
                key,
                fallback_receiver,
            )?))
        }
    }

    pub(super) fn proxy_set(
        &mut self,
        document: &mut BaseDocument,
        proxy: &Value,
        key: &str,
        value: Value,
    ) -> Result<bool> {
        self.proxy_set_with_receiver(document, proxy, key, value, proxy.clone())
    }

    pub(super) fn proxy_set_with_receiver(
        &mut self,
        document: &mut BaseDocument,
        proxy: &Value,
        key: &str,
        value: Value,
        receiver: Value,
    ) -> Result<bool> {
        if proxy.kind() != "Proxy" {
            return Ok(false);
        }
        let (target, handler) = read_state(proxy)?;
        let result = call_trap(
            self,
            document,
            &handler,
            "set",
            vec![
                target.clone(),
                super::exec::property_key_value(&self.global, key),
                value.clone(),
                receiver,
            ],
        )?;
        match result {
            Some(result) if !result.truthy() => Ok(false),
            Some(_) => {
                let descriptor = self.ordinary_descriptor(document, &target, key)?;
                if !matches!(&descriptor, Value::Undefined)
                    && !descriptor.get("configurable").truthy()
                    && !descriptor.get("writable").truthy()
                    && !value.same(&descriptor.get("value"))
                {
                    return Err(Error::type_error(
                        "Proxy set trap changed a frozen data property",
                    ));
                }
                Ok(true)
            }
            None => {
                self.set_property(document, &target, key, value)?;
                Ok(true)
            }
        }
    }

    pub(super) fn proxy_has(
        &mut self,
        document: &mut BaseDocument,
        proxy: &Value,
        key: &str,
    ) -> Result<Option<bool>> {
        if proxy.kind() != "Proxy" {
            return Ok(None);
        }
        let (target, handler) = read_state(proxy)?;
        if let Some(value) = call_trap(
            self,
            document,
            &handler,
            "has",
            vec![
                target.clone(),
                super::exec::property_key_value(&self.global, key),
            ],
        )? {
            let present = value.truthy();
            if !present {
                let descriptor = self.ordinary_descriptor(document, &target, key)?;
                let frozen = target
                    .as_object()
                    .is_some_and(|object| object.borrow().frozen);
                if !matches!(&descriptor, Value::Undefined)
                    && (!descriptor.get("configurable").truthy() || frozen)
                {
                    return Err(Error::type_error(
                        "Proxy has trap hid a non-configurable target property",
                    ));
                }
            }
            return Ok(Some(present));
        }
        Ok(Some(self.has_property(document, &target, key)?))
    }

    pub(super) fn proxy_apply(
        &mut self,
        document: &mut BaseDocument,
        proxy: &Value,
        this: Value,
        args: Vec<Value>,
        construct: bool,
    ) -> Result<Option<Value>> {
        if proxy.kind() != "Proxy" {
            return Ok(None);
        }
        let (target, handler) = read_state(proxy)?;
        if construct && !is_callable(&target) {
            return Err(Error::type_error("Proxy target is not a constructor"));
        }
        if !construct && !is_callable(&target) {
            return Err(Error::type_error("Proxy target is not callable"));
        }
        let key = if construct { "construct" } else { "apply" };
        let list = Value::array(args.clone());
        let trap_args = if construct {
            vec![target.clone(), list, proxy.clone()]
        } else {
            vec![target.clone(), this.clone(), list]
        };
        if let Some(result) = call_trap(self, document, &handler, key, trap_args)? {
            if construct && result.as_object().is_none() {
                return Err(Error::type_error(
                    "Proxy construct trap must return an object",
                ));
            }
            return Ok(Some(result));
        }
        Ok(Some(self.call(
            document,
            target,
            if construct { Value::Undefined } else { this },
            args,
            construct,
        )?))
    }

    pub(super) fn proxy_own_keys(
        &mut self,
        document: &mut BaseDocument,
        proxy: &Value,
    ) -> Result<Option<Vec<String>>> {
        if proxy.kind() != "Proxy" {
            return Ok(None);
        }
        let (target, handler) = read_state(proxy)?;
        let Some(result) = call_trap(self, document, &handler, "ownKeys", vec![target.clone()])?
        else {
            return Ok(Some(self.own_property_names(document, &target)?));
        };
        if result.as_object().is_none() {
            return Err(Error::type_error(
                "Proxy ownKeys trap must return an array-like object",
            ));
        }
        let length = self
            .get_property(document, &result, "length")?
            .number_value();
        if length == f64::INFINITY || length > super::exec::MAX_ARRAY_LENGTH as f64 {
            return Err(Error::new(
                "RangeError",
                "Proxy ownKeys trap result exceeds the property-key limit",
            ));
        }
        let length = if !length.is_finite() || length <= 0.0 {
            0
        } else {
            length.floor() as usize
        };
        let mut keys = Vec::with_capacity(length);
        for index in 0..length {
            let value = self.get_property(document, &result, &index.to_string())?;
            if !matches!(value.kind().as_str(), "String" | "Symbol") {
                return Err(Error::type_error(format!(
                    "Proxy ownKeys trap entry {index} must be a string or symbol, got {}",
                    value.kind()
                )));
            }
            let key = super::exec::property_key(&value);
            if keys.contains(&key) {
                return Err(Error::type_error(
                    "Proxy ownKeys trap returned duplicate keys",
                ));
            }
            keys.push(key);
        }
        let target_keys = self.own_property_names(document, &target)?;
        if let Some(object) = target.as_object() {
            let frozen = object.borrow().frozen;
            for key in &target_keys {
                let descriptor = self.ordinary_descriptor(document, &target, key)?;
                if !descriptor.get("configurable").truthy() && !keys.contains(key) {
                    return Err(Error::type_error(
                        "Proxy ownKeys trap omitted a non-configurable property",
                    ));
                }
            }
            if frozen
                && (keys.len() != target_keys.len()
                    || target_keys.iter().any(|key| !keys.contains(key)))
            {
                return Err(Error::type_error(
                    "Proxy ownKeys trap violated target property invariants",
                ));
            }
        }
        Ok(Some(keys))
    }

    pub(super) fn proxy_descriptor(
        &mut self,
        document: &mut BaseDocument,
        proxy: &Value,
        key: &str,
    ) -> Result<Option<Value>> {
        if proxy.kind() != "Proxy" {
            return Ok(None);
        }
        let (target, handler) = read_state(proxy)?;
        if let Some(result) = call_trap(
            self,
            document,
            &handler,
            "getOwnPropertyDescriptor",
            vec![
                target.clone(),
                super::exec::property_key_value(&self.global, key),
            ],
        )? {
            if matches!(result, Value::Undefined | Value::Null | Value::Bool(false)) {
                return Ok(Some(Value::Undefined));
            }
            if result.as_object().is_none() {
                return Err(Error::type_error(
                    "Proxy getOwnPropertyDescriptor trap must return an object or undefined",
                ));
            }
            return Ok(Some(result));
        }
        Ok(Some(self.ordinary_descriptor(document, &target, key)?))
    }

    pub(super) fn proxy_delete(
        &mut self,
        document: &mut BaseDocument,
        proxy: &Value,
        key: &str,
    ) -> Result<Option<bool>> {
        if proxy.kind() != "Proxy" {
            return Ok(None);
        }
        let (target, handler) = read_state(proxy)?;
        if let Some(result) = call_trap(
            self,
            document,
            &handler,
            "deleteProperty",
            vec![
                target.clone(),
                super::exec::property_key_value(&self.global, key),
            ],
        )? {
            if result.truthy() {
                let descriptor = self.ordinary_descriptor(document, &target, key)?;
                if !matches!(&descriptor, Value::Undefined)
                    && !descriptor.get("configurable").truthy()
                {
                    return Err(Error::type_error(
                        "Proxy deleteProperty trap deleted a non-configurable target property",
                    ));
                }
            }
            return Ok(Some(result.truthy()));
        }
        if let Some(object) = target.as_object() {
            let existed = object.borrow().props.contains_key(key)
                || key
                    .parse::<usize>()
                    .ok()
                    .is_some_and(|index| index < object.borrow().indexed.len());
            if existed
                && (object.borrow().frozen || (key == "length" && object.borrow().kind == "Array"))
            {
                return Err(Error::type_error(
                    "cannot delete a non-configurable Proxy target property",
                ));
            }
            let mut object = object.borrow_mut();
            object.props.remove(key);
            if let Ok(index) = key.parse::<usize>() {
                if let Some(value) = object.indexed.get_mut(index) {
                    *value = Value::Undefined;
                }
            }
            return Ok(Some(true));
        }
        Ok(Some(true))
    }

    pub(super) fn proxy_define(
        &mut self,
        document: &mut BaseDocument,
        proxy: &Value,
        key: &str,
        descriptor: &Value,
    ) -> Result<Option<bool>> {
        if proxy.kind() != "Proxy" {
            return Ok(None);
        }
        let (target, handler) = read_state(proxy)?;
        if let Some(result) = call_trap(
            self,
            document,
            &handler,
            "defineProperty",
            vec![
                target.clone(),
                super::exec::property_key_value(&self.global, key),
                descriptor.clone(),
            ],
        )? {
            return Ok(Some(result.truthy()));
        }
        let Some(object) = target.as_object() else {
            return Ok(Some(false));
        };
        let value = descriptor.get("value");
        let get = descriptor.get("get");
        let set = descriptor.get("set");
        let enumerable = descriptor.get("enumerable").truthy();
        let mut object = object.borrow_mut();
        if object.frozen && !object.props.contains_key(key) {
            return Ok(Some(false));
        }
        object.props.insert(
            key.into(),
            Property {
                value,
                get: (!matches!(get, Value::Undefined)).then_some(get),
                set: (!matches!(set, Value::Undefined)).then_some(set),
                enumerable,
            },
        );
        Ok(Some(true))
    }
}

pub(super) fn proxy_construct(target: Value, handler: Value) -> Result<Value> {
    create(target, handler)
}

pub(super) fn call_revoke(global: &Value, name: &str) -> Result<Value> {
    let id = name
        .strip_prefix("Proxy.revoke.")
        .ok_or_else(|| Error::type_error("invalid Proxy revoker"))?;
    let registry = global.get("\0proxy_revokers");
    let proxy = registry.get(id);
    if proxy.kind() == "Undefined" {
        return Ok(Value::Undefined);
    }
    revoke(&proxy);
    if let Some(object) = registry.as_object() {
        object.borrow_mut().props.remove(id);
    }
    Ok(Value::Undefined)
}

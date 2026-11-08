//! TreeWalker over the live DOM. Only node identities outlive a guest call.

use std::cell::Cell;
use std::rc::Rc;

use super::{BaseDocument, Error, Guest, NodeData, NodeId, Property, Result, RuntimeBridge, Value};
use crate::native::proxy::is_callable;

pub(super) const ACCEPT: u16 = 1;
pub(super) const REJECT: u16 = 2;
pub(super) const SKIP: u16 = 3;

struct TreeWalker {
    root: NodeId,
    current: Cell<NodeId>,
    what_to_show: u32,
    filter: Value,
    active: Cell<bool>,
}

impl TreeWalker {
    fn from_value(value: &Value, realm: u64) -> Option<Rc<Self>> {
        let object = value.as_object()?;
        let object = object.borrow();
        if object.realm != Some(realm) {
            return None;
        }
        object.payload.clone()?.downcast().ok()
    }
}

impl<B: RuntimeBridge> Guest<B> {
    pub(super) fn install_tree_walker(&mut self) {
        let constructor = self.global.get("TreeWalker");
        let prototype = constructor.get("prototype");
        prototype.put("constructor", constructor);
        for method in [
            "parentNode",
            "firstChild",
            "lastChild",
            "previousSibling",
            "nextSibling",
            "previousNode",
            "nextNode",
        ] {
            prototype.put(method, Value::native(format!("DOM.TreeWalker.{method}")));
        }
        let object = prototype.as_object().unwrap();
        let mut object = object.borrow_mut();
        for key in ["root", "currentNode", "whatToShow", "filter"] {
            object.props.insert(
                key.into(),
                Property {
                    value: Value::Undefined,
                    get: Some(Value::native(format!("DOM.TreeWalker.get.{key}"))),
                    set: (key == "currentNode")
                        .then(|| Value::native("DOM.TreeWalker.set.currentNode")),
                    enumerable: true,
                },
            );
        }
        self.global.get("Document").get("prototype").put(
            "createTreeWalker",
            Value::native("DOM.createTreeWalker"),
        );
    }

    pub(super) fn tree_walker_readonly(&self, receiver: &Value, key: &str) -> bool {
        matches!(key, "root" | "whatToShow" | "filter")
            && TreeWalker::from_value(receiver, self.scope_id).is_some()
    }

    fn tree_walker_node(&self, doc: &BaseDocument, value: &Value) -> Result<NodeId> {
        value
            .node_id()
            .filter(|id| {
                self.nodes.get(id).is_some_and(|node| node.same(value))
                    && doc.get_node(*id).is_some()
            })
            .ok_or_else(|| Error::type_error("Expected a live DOM node"))
    }

    pub(super) fn create_tree_walker(
        &mut self,
        doc: &mut BaseDocument,
        receiver: &Value,
        args: &[Value],
    ) -> Result<Value> {
        if self.tree_walker_node(doc, receiver)? != doc.root_node().id {
            return Err(Error::type_error("Expected a Document receiver"));
        }
        let root = self.tree_walker_node(doc, &super::arg(args, 0))?;
        let what_to_show = match args.get(1) {
            None | Some(Value::Undefined) => u32::MAX,
            Some(mask) => self.tree_walker_unsigned(doc, mask.clone(), 4_294_967_296.0)?,
        };
        let filter = match super::arg(args, 2) {
            Value::Null | Value::Undefined => Value::Null,
            filter @ Value::Object(_) if filter.kind() != "Symbol" => filter,
            _ => return Err(Error::type_error("NodeFilter must be an object or null")),
        };
        let walker = Value::object("TreeWalker");
        let object = walker.as_object().unwrap();
        let mut object = object.borrow_mut();
        object.prototype = Some(self.global.get("TreeWalker").get("prototype"));
        object.payload = Some(Rc::new(TreeWalker {
            root,
            current: Cell::new(root),
            what_to_show,
            filter,
            active: Cell::new(false),
        }));
        Ok(walker)
    }

    pub(super) fn tree_walker_call(
        &mut self,
        doc: &mut BaseDocument,
        receiver: &Value,
        method: &str,
        args: &[Value],
    ) -> Result<Value> {
        let walker = TreeWalker::from_value(receiver, self.scope_id)
            .ok_or_else(|| Error::type_error("Expected a TreeWalker receiver"))?;
        match method {
            "get.root" => return Ok(self.node(walker.root)),
            "get.currentNode" => return Ok(self.node(walker.current.get())),
            "get.whatToShow" => return Ok(Value::number(walker.what_to_show)),
            "get.filter" => return Ok(walker.filter.clone()),
            "set.currentNode" => {
                walker
                    .current
                    .set(self.tree_walker_node(doc, &super::arg(args, 0))?);
                return Ok(Value::Undefined);
            }
            _ => {}
        }
        let mut traversal = Traversal {
            guest: self,
            doc,
            walker: &walker,
        };
        let current = walker.current.get();
        let found = match method {
            "parentNode" => traversal.parent_node(current)?,
            "firstChild" => traversal.child_node(current, Direction::Forward)?,
            "lastChild" => traversal.child_node(current, Direction::Backward)?,
            "nextSibling" => traversal.sibling_node(current, Direction::Forward)?,
            "previousSibling" => traversal.sibling_node(current, Direction::Backward)?,
            "nextNode" => traversal.next_node(current)?,
            "previousNode" => traversal.previous_node(current)?,
            _ => return Err(Error::type_error("Unknown TreeWalker method")),
        };
        if let Some(node) = found {
            walker.current.set(node);
            Ok(self.typed_node(doc, node))
        } else {
            Ok(Value::Null)
        }
    }

    fn tree_walker_unsigned(
        &mut self,
        doc: &mut BaseDocument,
        mut value: Value,
        modulus: f64,
    ) -> Result<u32> {
        if matches!(value, Value::Object(_)) && value.kind() != "Symbol" {
            let mut primitive = None;
            for key in ["valueOf", "toString"] {
                let method = self.get_property(doc, &value, key)?;
                if is_callable(&method) {
                    let result = self.call(doc, method, value.clone(), Vec::new(), false)?;
                    if !matches!(result, Value::Object(_)) || result.kind() == "Symbol" {
                        primitive = Some(result);
                        break;
                    }
                }
            }
            value = primitive.ok_or_else(|| Error::type_error("Cannot convert object to a number"))?;
        }
        if matches!(value, Value::Object(_)) {
            return Err(Error::type_error("Cannot convert a Symbol to a number"));
        }
        // Casting NaN (including infinity modulo a finite value) yields zero.
        Ok(value.number_value().trunc().rem_euclid(modulus) as u32)
    }
}

#[derive(Clone, Copy)]
enum Direction {
    Forward,
    Backward,
}

struct Traversal<'a, B: RuntimeBridge> {
    guest: &'a mut Guest<B>,
    doc: &'a mut BaseDocument,
    walker: &'a TreeWalker,
}

impl<B: RuntimeBridge> Traversal<'_, B> {
    fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.doc.get_node(id)?.parent
    }

    fn child(&self, id: NodeId, direction: Direction) -> Option<NodeId> {
        let node = self.doc.get_node(id)?;
        let index = match direction {
            Direction::Forward => 0,
            Direction::Backward => node.children.len().checked_sub(1)?,
        };
        node.dom_child_at(index)
    }

    fn sibling(&self, id: NodeId, direction: Direction) -> Option<NodeId> {
        // Use the DOM node's validated child-index hint when walking wide lists.
        let node = self.doc.get_node(id)?;
        match direction {
            Direction::Forward => node.forward(1),
            Direction::Backward => node.backward(1),
        }
        .map(|node| node.id)
    }

    fn filter_node(&mut self, id: NodeId) -> Result<u16> {
        self.guest.tick()?;
        if self.walker.active.get() {
            return Err(Error::new(
                "InvalidStateError",
                "Recursive NodeFilter invocation",
            ));
        }
        let bit = match self.doc.get_node(id).map(|node| &node.data) {
            Some(NodeData::Element(_) | NodeData::AnonymousBlock(_)) => 1,
            Some(NodeData::Text(_)) => 4,
            Some(NodeData::Comment { .. }) => 128,
            Some(NodeData::Document(_)) => 256,
            Some(NodeData::DocumentFragment | NodeData::ShadowRoot(_)) => 1024,
            None => 0,
        };
        if self.walker.what_to_show & bit == 0 {
            return Ok(SKIP);
        }
        let filter = &self.walker.filter;
        if matches!(filter, Value::Null) {
            return Ok(ACCEPT);
        }
        self.walker.active.set(true);
        // Keep no node or object borrow across user code.
        let result = (|| {
            let (callback, receiver) = if is_callable(filter) {
                (filter.clone(), Value::Undefined)
            } else {
                let callback = self.guest.get_property(self.doc, filter, "acceptNode")?;
                if !is_callable(&callback) {
                    return Err(Error::type_error("NodeFilter.acceptNode is not callable"));
                }
                (callback, filter.clone())
            };
            let node = self.guest.typed_node(self.doc, id);
            let result = self
                .guest
                .call(self.doc, callback, receiver, vec![node], false)?;
            Ok(self.guest.tree_walker_unsigned(self.doc, result, 65_536.0)? as u16)
        })();
        self.walker.active.set(false);
        result
    }

    fn parent_node(&mut self, mut node: NodeId) -> Result<Option<NodeId>> {
        while node != self.walker.root {
            let Some(parent) = self.parent(node) else {
                break;
            };
            node = parent;
            if self.filter_node(node)? == ACCEPT {
                return Ok(Some(node));
            }
        }
        Ok(None)
    }

    fn child_node(&mut self, current: NodeId, direction: Direction) -> Result<Option<NodeId>> {
        let mut candidate = self.child(current, direction);
        while let Some(node) = candidate {
            match self.filter_node(node)? {
                ACCEPT => return Ok(Some(node)),
                SKIP => {
                    if let Some(child) = self.child(node, direction) {
                        candidate = Some(child);
                        continue;
                    }
                }
                _ => {}
            }
            candidate = self.following(node, current, direction)?;
        }
        Ok(None)
    }

    fn sibling_node(&mut self, mut node: NodeId, direction: Direction) -> Result<Option<NodeId>> {
        while node != self.walker.root {
            let mut candidate = self.sibling(node, direction);
            while let Some(sibling) = candidate {
                node = sibling;
                let result = self.filter_node(node)?;
                if result == ACCEPT {
                    return Ok(Some(node));
                }
                candidate = if result == REJECT {
                    None
                } else {
                    self.child(node, direction)
                }
                .or_else(|| self.sibling(node, direction));
            }
            let Some(parent) = self.parent(node) else {
                break;
            };
            node = parent;
            if node == self.walker.root || self.filter_node(node)? == ACCEPT {
                break;
            }
        }
        Ok(None)
    }

    fn following(
        &mut self,
        mut node: NodeId,
        boundary: NodeId,
        direction: Direction,
    ) -> Result<Option<NodeId>> {
        while node != self.walker.root && node != boundary {
            self.guest.tick()?;
            if let Some(sibling) = self.sibling(node, direction) {
                return Ok(Some(sibling));
            }
            let Some(parent) = self.parent(node) else {
                break;
            };
            node = parent;
        }
        Ok(None)
    }

    fn next_node(&mut self, mut node: NodeId) -> Result<Option<NodeId>> {
        let mut result = ACCEPT;
        loop {
            let child = if result != REJECT {
                self.child(node, Direction::Forward)
            } else {
                None
            };
            let next = match child {
                Some(child) => Some(child),
                None => self.following(node, self.walker.root, Direction::Forward)?,
            };
            let Some(next) = next else {
                return Ok(None);
            };
            node = next;
            result = self.filter_node(node)?;
            if result == ACCEPT {
                return Ok(Some(node));
            }
        }
    }

    fn previous_node(&mut self, mut node: NodeId) -> Result<Option<NodeId>> {
        while node != self.walker.root {
            while let Some(sibling) = self.sibling(node, Direction::Backward) {
                node = sibling;
                let mut result = self.filter_node(node)?;
                while result != REJECT {
                    let Some(child) = self.child(node, Direction::Backward) else {
                        break;
                    };
                    node = child;
                    result = self.filter_node(node)?;
                }
                if result == ACCEPT {
                    return Ok(Some(node));
                }
            }
            let Some(parent) = self.parent(node) else {
                break;
            };
            node = parent;
            if self.filter_node(node)? == ACCEPT {
                return Ok(Some(node));
            }
        }
        Ok(None)
    }
}

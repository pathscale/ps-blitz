//! Native indexing and mutation records for script custom elements.
//!
//! This is independent of the embedder's Rust custom element controllers.
//! No JavaScript handles or callbacks are stored in the DOM.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::{BaseDocument, LocalName, NodeData, NodeId, QualName, ns};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Undefined,
    Upgrading,
    Custom,
    Failed,
}

struct Candidate {
    name: Arc<str>,
    local_name: LocalName,
    state: State,
}

struct Definition {
    local_name: LocalName,
    observed: Arc<HashSet<String>>,
}

#[derive(Clone)]
pub enum ReactionKind {
    Upgrade,
    Connected,
    Disconnected,
    Attribute {
        name: QualName,
        old_value: Option<String>,
        new_value: Option<String>,
    },
    Adopted {
        old_document: NodeId,
        new_document: NodeId,
    },
}

#[derive(Clone)]
pub struct Reaction {
    pub node: NodeId,
    pub name: Arc<str>,
    pub kind: ReactionKind,
}

#[derive(Default)]
pub struct Registry {
    candidates: HashMap<NodeId, Candidate>,
    names: HashMap<Arc<str>, HashSet<NodeId>>,
    definitions: HashMap<Arc<str>, Definition>,
    reactions: Vec<Reaction>,
}

impl BaseDocument {
    pub(crate) fn register_script_custom_element(&mut self, node_id: NodeId) {
        let Some(element) = self.nodes[node_id].element_data() else {
            return;
        };
        if element.name.ns != ns!(html) {
            return;
        }
        let name: Arc<str> = if element.name.local.as_ref().contains('-') {
            Arc::from(element.name.local.as_ref())
        } else if let Some(name) = element.attr(LocalName::from("is")) {
            Arc::from(name)
        } else {
            return;
        };
        let local_name = element.name.local.clone();
        self.nodes[node_id].custom_element_subtree_count = 1;
        self.script_custom_elements
            .names
            .entry(Arc::clone(&name))
            .or_default()
            .insert(node_id);
        self.script_custom_elements.candidates.insert(
            node_id,
            Candidate {
                name,
                local_name,
                state: State::Undefined,
            },
        );
        if self.script_custom_element_is_defined(node_id) {
            self.record_script_custom_element(node_id, ReactionKind::Upgrade);
        }
    }

    pub(crate) fn forget_script_custom_element(&mut self, node_id: NodeId) {
        if let Some(candidate) = self.script_custom_elements.candidates.remove(&node_id) {
            if let Some(nodes) = self.script_custom_elements.names.get_mut(&candidate.name) {
                nodes.remove(&node_id);
                if nodes.is_empty() {
                    self.script_custom_elements.names.remove(&candidate.name);
                }
            }
        }
    }

    pub fn script_custom_element_name(&self, node_id: NodeId) -> Option<&str> {
        self.script_custom_elements
            .candidates
            .get(&node_id)
            .map(|candidate| candidate.name.as_ref())
    }

    pub fn script_custom_element_state(&self, node_id: NodeId) -> State {
        self.script_custom_elements
            .candidates
            .get(&node_id)
            .map_or(State::Undefined, |candidate| candidate.state)
    }

    pub fn set_script_custom_element_state(&mut self, node_id: NodeId, state: State) {
        if let Some(candidate) = self.script_custom_elements.candidates.get_mut(&node_id) {
            candidate.state = state;
        }
    }

    pub fn script_custom_element_is_defined(&self, node_id: NodeId) -> bool {
        let Some(candidate) = self.script_custom_elements.candidates.get(&node_id) else {
            return false;
        };
        self.script_custom_elements
            .definitions
            .get(&candidate.name)
            .is_some_and(|definition| definition.local_name == candidate.local_name)
    }

    /// DOM connectivity includes shadow trees and documents in the shared arena.
    pub fn script_node_is_connected(&self, node_id: NodeId) -> bool {
        let mut current = Some(node_id);
        while let Some(id) = current {
            let Some(node) = self.get_node(id) else {
                return false;
            };
            if matches!(node.data, NodeData::Document(_)) {
                return true;
            }
            current = node.parent;
        }
        false
    }

    fn record_script_custom_element(&mut self, node_id: NodeId, kind: ReactionKind) {
        if let Some(candidate) = self.script_custom_elements.candidates.get(&node_id) {
            self.script_custom_elements.reactions.push(Reaction {
                node: node_id,
                name: Arc::clone(&candidate.name),
                kind,
            });
        }
    }

    pub fn take_script_custom_element_reactions(&mut self) -> Vec<Reaction> {
        std::mem::take(&mut self.script_custom_elements.reactions)
    }

    /// Register native matching metadata and return only this name's connected
    /// upgrade candidates, in shadow-including tree order.
    pub fn define_script_custom_element(
        &mut self,
        name: &str,
        local_name: LocalName,
        observed: Arc<HashSet<String>>,
    ) -> Vec<NodeId> {
        let name: Arc<str> = Arc::from(name);
        self.script_custom_elements.definitions.insert(
            Arc::clone(&name),
            Definition {
                local_name: local_name.clone(),
                observed,
            },
        );
        let mut matches: Vec<_> = self
            .script_custom_elements
            .names
            .get(&name)
            .into_iter()
            .flatten()
            .copied()
            .filter(|id| {
                let candidate = &self.script_custom_elements.candidates[id];
                candidate.state == State::Undefined
                    && candidate.local_name == local_name
                    && self.get_node(*id).is_some_and(|node| {
                        node.owner_document == Some(self.root_node_id)
                    })
                    && self.script_node_is_connected(*id)
            })
            .map(|id| (self.script_tree_order_key(id), id))
            .collect();
        matches.sort_by(|left, right| left.0.cmp(&right.0));
        matches.into_iter().map(|(_, id)| id).collect()
    }

    fn script_tree_order_key(&self, node_id: NodeId) -> Vec<usize> {
        let mut key = Vec::new();
        let mut current = node_id;
        while let Some(parent_id) = self.get_node(current).and_then(|node| node.parent) {
            let Some(parent) = self.get_node(parent_id) else {
                break;
            };
            // Shadow roots precede the host's light children.
            let index = parent
                .children
                .iter()
                .position(|id| *id == current)
                .map_or(0, |index| index + 1);
            key.push(index);
            current = parent_id;
        }
        key.reverse();
        key
    }

    /// Walk only branches containing candidates. Plain subtrees allocate
    /// nothing and do not require a traversal.
    pub fn script_custom_element_candidates(&self, root: NodeId) -> Vec<NodeId> {
        if self
            .get_node(root)
            .is_none_or(|node| node.custom_element_subtree_count == 0)
        {
            return Vec::new();
        }
        let mut found = Vec::new();
        let mut stack = vec![root];
        while let Some(id) = stack.pop() {
            let Some(node) = self.get_node(id) else {
                continue;
            };
            if node.custom_element_subtree_count == 0 {
                continue;
            }
            if self.script_custom_elements.candidates.contains_key(&id) {
                found.push(id);
            }
            stack.extend(node.children.iter().rev().copied());
            #[cfg(feature = "shadow-dom")]
            if let Some(shadow_root) = node.shadow_root_id() {
                stack.push(shadow_root);
            }
        }
        found
    }

    fn change_script_custom_element_count(
        &mut self,
        mut ancestor: Option<NodeId>,
        count: usize,
        add: bool,
    ) {
        if count == 0 {
            return;
        }
        while let Some(id) = ancestor {
            let Some(node) = self.get_node_mut(id) else {
                break;
            };
            if add {
                node.custom_element_subtree_count += count;
            } else {
                node.custom_element_subtree_count -= count;
            }
            ancestor = node.parent;
        }
    }

    /// Called before cutting a parent edge, including moves within one parent.
    pub(crate) fn detach_script_custom_element_subtree(&mut self, root: NodeId) {
        let Some(node) = self.get_node(root) else {
            return;
        };
        let count = node.custom_element_subtree_count;
        let parent = node.parent;
        if count == 0 {
            return;
        }
        if parent.is_some() && self.script_node_is_connected(root) {
            for id in self.script_custom_element_candidates(root) {
                if matches!(
                    self.script_custom_element_state(id),
                    State::Custom | State::Upgrading
                ) {
                    self.record_script_custom_element(id, ReactionKind::Disconnected);
                }
            }
        }
        self.change_script_custom_element_count(parent, count, false);
    }

    /// Called after establishing a new parent edge.
    pub(crate) fn attach_script_custom_element_subtree(
        &mut self,
        root: NodeId,
        parent: NodeId,
    ) {
        let owner = self.nodes[parent].owner_document.unwrap_or(parent);
        self.adopt_script_subtree(root, owner);
        let count = self.nodes[root].custom_element_subtree_count;
        self.change_script_custom_element_count(Some(parent), count, true);
        if count == 0 || !self.script_node_is_connected(parent) {
            return;
        }
        for id in self.script_custom_element_candidates(root) {
            match self.script_custom_element_state(id) {
                State::Custom | State::Upgrading => {
                    self.record_script_custom_element(id, ReactionKind::Connected);
                }
                State::Undefined
                    if self.nodes[id].owner_document == Some(self.root_node_id)
                        && self.script_custom_element_is_defined(id) =>
                {
                    self.record_script_custom_element(id, ReactionKind::Upgrade);
                }
                _ => {}
            }
        }
    }

    /// Owner changes cost one walk of the adopted subtree. Ordinary insertion
    /// into the same document only performs the owner comparison.
    pub fn adopt_script_subtree(&mut self, root: NodeId, new_document: NodeId) {
        if self
            .get_node(root)
            .is_none_or(|node| node.owner_document == Some(new_document))
        {
            return;
        }
        let mut stack = vec![root];
        while let Some(id) = stack.pop() {
            let Some(node) = self.get_node_mut(id) else {
                continue;
            };
            let old_document = node.owner_document;
            node.owner_document = Some(new_document);
            stack.extend(node.children.iter().rev().copied());
            #[cfg(feature = "shadow-dom")]
            if let Some(shadow_root) = node.shadow_root_id() {
                stack.push(shadow_root);
            }
            if matches!(
                self.script_custom_element_state(id),
                State::Custom | State::Upgrading
            ) && let Some(old_document) = old_document
                && old_document != new_document
            {
                self.record_script_custom_element(
                    id,
                    ReactionKind::Adopted {
                        old_document,
                        new_document,
                    },
                );
            }
        }
    }

    /// Capture the old value before the attribute store changes. Equal-value
    /// writes still react; removing an absent attribute does not.
    pub(crate) fn record_script_custom_element_attribute(
        &mut self,
        node_id: NodeId,
        name: &QualName,
        new_value: Option<&str>,
    ) {
        if !matches!(
            self.script_custom_element_state(node_id),
            State::Custom | State::Upgrading
        ) {
            return;
        }
        let candidate = &self.script_custom_elements.candidates[&node_id];
        let Some(definition) = self.script_custom_elements.definitions.get(&candidate.name) else {
            return;
        };
        let local: &str = name.local.as_ref();
        if !definition.observed.contains(local) {
            return;
        }
        let old_value = self.nodes[node_id]
            .element_data()
            .and_then(|element| element.attrs.iter().find(|attribute| attribute.name == *name))
            .map(|attribute| attribute.value.to_string());
        if old_value.is_none() && new_value.is_none() {
            return;
        }
        self.record_script_custom_element(
            node_id,
            ReactionKind::Attribute {
                name: name.clone(),
                old_value,
                new_value: new_value.map(str::to_owned),
            },
        );
    }
}


//! Native shadow-tree composition, stylesheet ownership and event boundaries.

use std::collections::HashMap;

use blitz_traits::node_id::NodeId;
use markup5ever::local_name;
use style::author_styles::AuthorStyles;
use style::invalidation::element::restyle_hints::RestyleHint;
use style::stylesheets::{
    AllowImportRules, DocumentStyleSheet, Origin, Stylesheet,
};

use crate::layout::damage::ALL_DAMAGE;
use crate::node::{ShadowRootMode, SpecialElementData};
use crate::BaseDocument;

impl BaseDocument {
    pub fn has_shadow_roots(&self) -> bool {
        !self.shadow_host_nodes.is_empty()
    }

    /// DOM ancestry stops at a shadow root. The internal root-to-host edge is
    /// reserved for shadow-including ancestry and native rendering.
    pub fn dom_parent_id(&self, node_id: NodeId) -> Option<NodeId> {
        let node = self.get_node(node_id)?;
        if node.is_shadow_root() {
            None
        } else {
            node.parent
        }
    }

    pub fn dom_root_id(&self, mut node_id: NodeId) -> NodeId {
        while let Some(parent_id) = self.dom_parent_id(node_id) {
            node_id = parent_id;
        }
        node_id
    }

    pub fn containing_shadow_root(&self, node_id: NodeId) -> Option<NodeId> {
        let root_id = self.dom_root_id(node_id);
        self.get_node(root_id)
            .filter(|node| node.is_shadow_root())
            .map(|node| node.id)
    }

    pub fn shadow_including_contains(&self, root_id: NodeId, mut node_id: NodeId) -> bool {
        loop {
            if node_id == root_id {
                return true;
            }
            let Some(parent_id) = self.get_node(node_id).and_then(|node| node.parent) else {
                return false;
            };
            node_id = parent_id;
        }
    }

    /// Called at native mutation points. Distribution is deferred until an
    /// assignment read, event dispatch, or rendering checkpoint.
    pub(crate) fn note_shadow_tree_change(&mut self, node_id: NodeId) {
        if self.shadow_host_nodes.is_empty() {
            return;
        }
        if let Some(root_id) = self.shadow_root_id(node_id) {
            self.dirty_shadow_hosts.insert(node_id);
            self.invalidate_shadow_styles(root_id);
        }
        if let Some(parent_id) = self.get_node(node_id).and_then(|node| node.parent) {
            if self.shadow_root_id(parent_id).is_some() {
                self.dirty_shadow_hosts.insert(parent_id);
            }
        }
        if let Some(root_id) = self.containing_shadow_root(node_id) {
            let host_id = self.nodes[root_id].shadow_root_data().unwrap().host;
            self.dirty_shadow_hosts.insert(host_id);
            self.invalidate_shadow_styles(root_id);
        }
        let is_slot = self
            .get_node(node_id)
            .is_some_and(|node| node.data.is_element_with_tag_name(&local_name!("slot")));
        if is_slot {
            self.signal_slot_change(node_id);
        }
    }

    fn signal_slot_change(&mut self, slot_id: NodeId) {
        if self.signaled_slots.insert(slot_id) {
            self.pending_slot_changes.push(slot_id);
        }
    }

    pub fn take_slot_changes(&mut self) -> Vec<NodeId> {
        self.compute_flattened_trees();
        self.signaled_slots.clear();
        let mut changes = std::mem::take(&mut self.pending_slot_changes);
        changes.retain(|id| self.get_node(*id).is_some());
        changes
    }

    /// Only dirty hosts are visited. Pages without roots take the empty-set
    /// path, and a batch of insertions distributes once.
    pub fn compute_flattened_trees(&mut self) {
        let hosts = std::mem::take(&mut self.dirty_shadow_hosts);
        for host_id in hosts {
            if self.get_node(host_id).is_some() {
                self.compute_flattened_tree_for_host(host_id);
            }
        }
    }

    fn compute_flattened_tree_for_host(&mut self, host_id: NodeId) {
        let Some(root_id) = self.shadow_root_id(host_id) else {
            return;
        };
        let old_slots = self.nodes[root_id].shadow_root_data().unwrap().slots.clone();
        let old_slottables = self.nodes[root_id]
            .shadow_root_data()
            .unwrap()
            .slottables
            .clone();

        for id in old_slottables {
            if let Some(node) = self.nodes.get_mut(id) {
                node.assigned_slot = None;
                if let Some(element) = node.element_data_mut() {
                    element.assigned_slot = None;
                }
            }
        }

        let mut slots = Vec::new();
        let mut stack: Vec<_> = self.nodes[root_id].children.iter().rev().copied().collect();
        while let Some(id) = stack.pop() {
            let Some(node) = self.get_node(id) else {
                continue;
            };
            if node.data.is_element_with_tag_name(&local_name!("slot")) {
                slots.push(id);
            }
            // This is a DOM walk. Nested shadow roots are not DOM children.
            stack.extend(node.children.iter().rev().copied());
        }

        let mut first_slot_by_name: HashMap<String, NodeId> = HashMap::new();
        let mut assignments: HashMap<NodeId, Vec<NodeId>> = HashMap::new();
        for &id in &slots {
            let name = self.nodes[id].attr(local_name!("name")).unwrap_or("");
            first_slot_by_name.entry(name.to_owned()).or_insert(id);
            assignments.insert(id, Vec::new());
        }

        let light_children = self.nodes[host_id].children.to_vec();
        let mut slottables = Vec::new();
        for &id in &light_children {
            let node = &self.nodes[id];
            if !node.is_element() && !node.is_text_node() {
                continue;
            }
            slottables.push(id);
            let name = node.attr(local_name!("slot")).unwrap_or("");
            let target = first_slot_by_name.get(name).copied();
            if let Some(slot_id) = target {
                assignments.get_mut(&slot_id).unwrap().push(id);
                self.nodes[id].assigned_slot = Some(slot_id);
                if let Some(element) = self.nodes[id].element_data_mut() {
                    element.assigned_slot = Some(slot_id);
                }
            }
        }

        let shadow_children = self.nodes[root_id].children.to_vec();
        if self.nodes[host_id].flattened_children.as_ref() != Some(&shadow_children) {
            self.nodes[host_id].flattened_children = Some(shadow_children);
            self.nodes[host_id].insert_damage(ALL_DAMAGE);
            self.nodes[host_id].set_restyle_hint(RestyleHint::restyle_subtree());
        }

        for &id in &old_slots {
            if !assignments.contains_key(&id) {
                if let Some(node) = self.nodes.get_mut(id) {
                    let changed = node.assigned_nodes.as_ref().is_some_and(|nodes| !nodes.is_empty());
                    node.assigned_nodes = None;
                    node.flattened_children = None;
                    if changed {
                        self.signal_slot_change(id);
                    }
                }
            }
        }

        for id in slots.iter().copied() {
            let assigned = assignments.remove(&id).unwrap_or_default();
            let changed = self.nodes[id].assigned_nodes.as_deref().unwrap_or(&[]) != assigned;
            self.nodes[id].flattened_children = if assigned.is_empty() {
                None
            } else {
                Some(assigned.clone())
            };
            self.nodes[id].assigned_nodes = Some(assigned);
            if changed {
                self.signal_slot_change(id);
                self.nodes[id].insert_damage(ALL_DAMAGE);
                self.nodes[id].set_restyle_hint(RestyleHint::restyle_subtree());
                self.nodes[host_id].insert_damage(ALL_DAMAGE);
                self.nodes[host_id].set_restyle_hint(RestyleHint::restyle_subtree());
            }
        }

        let root = self.nodes[root_id].shadow_root_data_mut().unwrap();
        root.slots = slots;
        root.slottables = slottables;
    }

    pub fn slot_assigned_nodes(&mut self, slot_id: NodeId, flatten: bool) -> Vec<NodeId> {
        self.compute_flattened_trees();
        let Some(slot) = self.get_node(slot_id) else {
            return Vec::new();
        };
        if !slot.data.is_element_with_tag_name(&local_name!("slot")) {
            return Vec::new();
        }
        let assigned = slot.assigned_nodes.clone().unwrap_or_default();
        if !flatten {
            return assigned;
        }

        let initial = if assigned.is_empty() {
            slot.children.to_vec()
        } else {
            assigned
        };
        let mut result = Vec::new();
        let mut stack: Vec<_> = initial.into_iter().rev().collect();
        while let Some(id) = stack.pop() {
            let Some(node) = self.get_node(id) else {
                continue;
            };
            if node.data.is_element_with_tag_name(&local_name!("slot"))
                && self.containing_shadow_root(id).is_some()
            {
                let children = node
                    .assigned_nodes
                    .as_ref()
                    .filter(|nodes| !nodes.is_empty())
                    .map(Vec::as_slice)
                    .unwrap_or(&node.children);
                stack.extend(children.iter().rev().copied());
            } else if node.is_element() || node.is_text_node() {
                result.push(id);
            }
        }
        result
    }

    pub(crate) fn invalidate_shadow_styles(&mut self, root_id: NodeId) {
        let Some(root) = self.nodes.get_mut(root_id).and_then(|node| node.shadow_root_data_mut()) else {
            return;
        };
        root.cascade_dirty = true;
        let host_id = root.host;
        if let Some(host) = self.nodes.get_mut(host_id) {
            host.set_restyle_hint(RestyleHint::restyle_subtree());
        }
    }

    pub(crate) fn install_shadow_stylesheet(
        &mut self,
        root_id: NodeId,
        node_id: NodeId,
        sheet: DocumentStyleSheet,
    ) {
        crate::net::fetch_font_face(
            self.tx.clone(),
            self.id(),
            Some(node_id),
            &sheet.0,
            &self.net_provider,
            &self.shell_provider,
            &self.guard.read(),
            self.abort_signal.as_ref(),
        );
        self.nodes_to_stylesheet.insert(node_id, sheet.clone());
        self.nodes[node_id].element_data_mut().unwrap().special_data =
            SpecialElementData::Stylesheet(sheet);
        self.invalidate_shadow_styles(root_id);
    }

    pub(crate) fn flush_shadow_styles(&mut self) {
        for host_id in self.shadow_host_node_ids() {
            let Some(root_id) = self.shadow_root_id(host_id) else {
                continue;
            };
            if !self.nodes[root_id].shadow_root_data().unwrap().cascade_dirty {
                continue;
            }

            let mut owners = Vec::new();
            let mut sheets = Vec::new();
            let mut stack: Vec<_> = self.nodes[root_id].children.iter().rev().copied().collect();
            while let Some(id) = stack.pop() {
                let node = &self.nodes[id];
                let is_style = node.data.is_element_with_tag_name(&local_name!("style"));
                if is_style || node.data.is_element_with_tag_name(&local_name!("link")) {
                    if let Some(sheet) = self.nodes_to_stylesheet.get(&id) {
                        owners.push(id);
                        sheets.push(sheet.clone());
                    } else if is_style {
                        owners.push(id);
                        sheets.push(self.make_stylesheet(node.text_content(), Origin::Author));
                    }
                }
                stack.extend(node.children.iter().rev().copied());
            }
            if let Some(adopted) = self.adopted_stylesheets.get(&root_id) {
                for sheet in adopted {
                    if !sheets.contains(sheet) {
                        sheets.push(sheet.clone());
                    }
                }
            }

            let mut author = AuthorStyles::<DocumentStyleSheet>::new();
            {
                let guard = self.guard.read();
                for sheet in sheets {
                    author.stylesheets.append_stylesheet(
                        None,
                        author.data.custom_media_map(),
                        sheet,
                        &guard,
                    );
                }
                author.flush(&mut self.stylist, &guard);
            }
            let root = self.nodes[root_id].shadow_root_data_mut().unwrap();
            root.stylesheet_nodes = owners;
            root.cascade_data = Some(author.data);
            root.cascade_dirty = false;
        }
    }

    pub fn make_constructed_stylesheet(&self, css: &str) -> DocumentStyleSheet {
        DocumentStyleSheet(style::servo_arc::Arc::new(Stylesheet::from_str(
            css,
            self.url.url_extra_data(),
            Origin::Author,
            style::servo_arc::Arc::new(self.guard.wrap(style::media_queries::MediaList::empty())),
            self.guard.clone(),
            None,
            None,
            style::context::QuirksMode::NoQuirks,
            AllowImportRules::No,
        )))
    }

    pub fn set_adopted_stylesheets(&mut self, root_id: NodeId, sheets: Vec<DocumentStyleSheet>) {
        let previous = self.adopted_stylesheets.insert(root_id, sheets.clone()).unwrap_or_default();
        if root_id == self.root_node_id {
            let guard = self.guard.read();
            let mut removed = Vec::new();
            for sheet in previous {
                if !removed.contains(&sheet) {
                    self.stylist.remove_stylesheet(sheet.clone(), &guard);
                    removed.push(sheet);
                }
            }
            let mut added = Vec::new();
            for sheet in &sheets {
                if !added.contains(sheet) {
                    self.stylist.append_stylesheet(sheet.clone(), &guard);
                    added.push(sheet.clone());
                }
            }
            drop(guard);
            if let Some(root) = self.try_root_element().map(|node| node.id) {
                self.nodes[root].set_restyle_hint(RestyleHint::restyle_subtree());
            }
        } else {
            self.invalidate_shadow_styles(root_id);
        }
        for sheet in sheets {
            crate::net::fetch_font_face(
                self.tx.clone(),
                self.id(),
                None,
                &sheet.0,
                &self.net_provider,
                &self.shell_provider,
                &self.guard.read(),
                self.abort_signal.as_ref(),
            );
        }
        self.shell_provider.request_redraw();
    }

    pub fn constructed_stylesheet_changed(&mut self, sheet: &DocumentStyleSheet) {
        let owners: Vec<_> = self
            .adopted_stylesheets
            .iter()
            .filter(|(_, sheets)| sheets.contains(sheet))
            .map(|(&id, _)| id)
            .collect();
        for owner_id in owners {
            if owner_id == self.root_node_id {
                self.stylist.force_stylesheet_origins_dirty(style::stylesheets::OriginSet::all());
                if let Some(root) = self.try_root_element().map(|node| node.id) {
                    self.nodes[root].set_restyle_hint(RestyleHint::restyle_subtree());
                }
            } else {
                self.invalidate_shadow_styles(owner_id);
            }
        }
        self.shell_provider.request_redraw();
    }

    pub fn shadow_event_path(&mut self, target_id: NodeId, composed: bool) -> Vec<NodeId> {
        self.compute_flattened_trees();
        let origin_root = self.dom_root_id(target_id);
        let mut path = Vec::new();
        let mut current = Some(target_id);
        while let Some(id) = current {
            let Some(node) = self.get_node(id) else {
                break;
            };
            path.push(id);
            current = if let Some(root) = node.shadow_root_data() {
                if !composed && id == origin_root {
                    None
                } else {
                    Some(root.host)
                }
            } else {
                node.assigned_slot.or(node.parent)
            };
        }
        path
    }

    pub fn retarget_shadow_event(&self, mut target_id: NodeId, observer_id: NodeId) -> NodeId {
        loop {
            let Some(root_id) = self.containing_shadow_root(target_id) else {
                return target_id;
            };
            if self.shadow_including_contains(root_id, observer_id) {
                return target_id;
            }
            target_id = self.nodes[root_id].shadow_root_data().unwrap().host;
        }
    }

    pub fn visible_shadow_event_path(&self, path: &[NodeId], observer_id: NodeId) -> Vec<NodeId> {
        path.iter()
            .copied()
            .filter(|&id| {
                let mut current = id;
                while let Some(root_id) = self.containing_shadow_root(current) {
                    let root = self.nodes[root_id].shadow_root_data().unwrap();
                    if root.mode == ShadowRootMode::Closed
                        && !self.shadow_including_contains(root_id, observer_id)
                    {
                        return false;
                    }
                    current = root.host;
                }
                true
            })
            .collect()
    }
}


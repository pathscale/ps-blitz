//! Live DOM ranges. Boundary offsets are UTF-16 code units for CharacterData
//! and child indices for other nodes. The mutation hooks also cover detached
//! trees and documents sharing this arena.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};

use crate::document::BoundingRect;
use crate::{BaseDocument, NodeData, NodeId, NodeTree};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RangeBoundary {
    pub node: NodeId,
    pub offset: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RangeBounds {
    pub start: RangeBoundary,
    pub end: RangeBoundary,
}

impl RangeBounds {
    pub fn collapsed(self) -> bool {
        self.start == self.end
    }
}

#[derive(Clone, Debug)]
pub struct LiveRange(pub(crate) Arc<Mutex<RangeBounds>>);

impl LiveRange {
    pub fn bounds(&self) -> RangeBounds {
        *self.0.lock().unwrap()
    }

    pub fn set_bounds(&self, bounds: RangeBounds) {
        *self.0.lock().unwrap() = bounds;
    }

    pub fn same_range(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

#[derive(Clone, Debug)]
pub enum RangeContent {
    Full(NodeId),
    Partial(NodeId, Vec<RangeContent>),
    Data(NodeId, usize, usize),
}

pub fn utf16_slice(value: &str, start: usize, end: usize) -> String {
    let units: Vec<u16> = value
        .encode_utf16()
        .skip(start)
        .take(end.saturating_sub(start))
        .collect();
    String::from_utf16_lossy(&units)
}

fn contains(nodes: &NodeTree, ancestor: NodeId, mut descendant: NodeId) -> bool {
    loop {
        if ancestor == descendant {
            return true;
        }
        let Some(parent) = nodes.get(descendant).and_then(|node| node.parent) else {
            return false;
        };
        descendant = parent;
    }
}

impl BaseDocument {
    pub fn create_live_range(&mut self, bounds: RangeBounds) -> LiveRange {
        self.live_ranges.retain(|range| range.strong_count() != 0);
        let range = LiveRange(Arc::new(Mutex::new(bounds)));
        self.live_ranges.push(Arc::downgrade(&range.0));
        range
    }

    pub fn range_character_data(&self, id: NodeId) -> Option<&str> {
        match &self.get_node(id)?.data {
            NodeData::Text(text) => Some(&text.content),
            NodeData::Comment { contents } => Some(contents),
            _ => None,
        }
    }

    pub fn range_node_length(&self, id: NodeId) -> Option<usize> {
        let node = self.get_node(id)?;
        Some(match self.range_character_data(id) {
            Some(text) => text.encode_utf16().count(),
            None => node.children.len(),
        })
    }

    pub fn range_contains(&self, ancestor: NodeId, descendant: NodeId) -> bool {
        contains(&self.nodes, ancestor, descendant)
    }

    fn range_path(&self, mut id: NodeId) -> Vec<NodeId> {
        let mut path = vec![id];
        while let Some(parent) = self.get_node(id).and_then(|node| node.parent) {
            path.push(parent);
            id = parent;
        }
        path.reverse();
        path
    }

    pub fn range_root(&self, id: NodeId) -> NodeId {
        self.range_path(id)[0]
    }

    pub fn range_common_ancestor(&self, bounds: RangeBounds) -> Option<NodeId> {
        self.range_path(bounds.start.node)
            .iter()
            .zip(self.range_path(bounds.end.node))
            .take_while(|(left, right)| **left == *right)
            .map(|(left, _)| *left)
            .last()
    }

    pub fn compare_range_boundaries(
        &self,
        left: RangeBoundary,
        right: RangeBoundary,
    ) -> Option<Ordering> {
        if left.node == right.node {
            return Some(left.offset.cmp(&right.offset));
        }
        let a = self.range_path(left.node);
        let b = self.range_path(right.node);
        if a.first() != b.first() {
            return None;
        }
        let common = a.iter().zip(&b).take_while(|(a, b)| a == b).count();
        let parent = self.get_node(a[common - 1])?;
        if common == a.len() {
            let index = parent.index_of_child(b[common])?;
            return Some(if index < left.offset {
                Ordering::Greater
            } else {
                Ordering::Less
            });
        }
        if common == b.len() {
            let index = parent.index_of_child(a[common])?;
            return Some(if index < right.offset {
                Ordering::Less
            } else {
                Ordering::Greater
            });
        }
        Some(
            parent
                .index_of_child(a[common])?
                .cmp(&parent.index_of_child(b[common])?),
        )
    }

    fn update_live_ranges(&mut self, mut update: impl FnMut(&mut RangeBoundary)) {
        self.live_ranges.retain(|weak| {
            let Some(range) = weak.upgrade() else {
                return false;
            };
            let mut bounds = range.lock().unwrap();
            update(&mut bounds.start);
            update(&mut bounds.end);
            true
        });
    }

    pub(crate) fn range_remove_node(&mut self, id: NodeId) {
        if self.live_ranges.is_empty() {
            return;
        }
        let Some(parent) = self.get_node(id).and_then(|node| node.parent) else {
            return;
        };
        let Some(index) = self
            .get_node(parent)
            .and_then(|node| node.index_of_child(id))
        else {
            return;
        };
        let nodes = &self.nodes;
        self.live_ranges.retain(|weak| {
            let Some(range) = weak.upgrade() else {
                return false;
            };
            let mut guard = range.lock().unwrap();
            let bounds = &mut *guard;
            for point in [&mut bounds.start, &mut bounds.end] {
                if contains(nodes, id, point.node) {
                    *point = RangeBoundary {
                        node: parent,
                        offset: index,
                    };
                } else if point.node == parent && point.offset > index {
                    point.offset -= 1;
                }
            }
            true
        });
    }

    pub(crate) fn range_insert_nodes(&mut self, parent: NodeId, index: usize, count: usize) {
        if count == 0 || self.live_ranges.is_empty() {
            return;
        }
        self.update_live_ranges(|point| {
            if point.node == parent && point.offset > index {
                point.offset += count;
            }
        });
    }

    pub(crate) fn range_replace_data(
        &mut self,
        id: NodeId,
        offset: usize,
        removed: usize,
        added: usize,
    ) {
        if self.live_ranges.is_empty() {
            return;
        }
        self.update_live_ranges(|point| {
            if point.node != id {
                return;
            }
            if point.offset > offset && point.offset <= offset + removed {
                point.offset = offset;
            } else if point.offset > offset + removed {
                point.offset = point.offset - removed + added;
            }
        });
    }

    pub(crate) fn range_split_text(
        &mut self,
        old: NodeId,
        new: NodeId,
        offset: usize,
        parent_position: Option<(NodeId, usize)>,
    ) {
        self.update_live_ranges(|point| {
            if point.node == old && point.offset > offset {
                point.node = new;
                point.offset -= offset;
            } else if let Some((parent, index)) = parent_position
                && point.node == parent
                && point.offset == index + 1
            {
                point.offset += 1;
            }
        });
    }

    /// Normalize transfers endpoints before the merged Text node is removed.
    pub fn range_merge_text(&mut self, keeper: NodeId, removed: NodeId, prefix: usize) {
        let position = self.get_node(removed).and_then(|node| {
            let parent = node.parent?;
            Some((parent, self.get_node(parent)?.index_of_child(removed)?))
        });
        self.update_live_ranges(|point| {
            if point.node == removed {
                point.node = keeper;
                point.offset += prefix;
            } else if let Some((parent, index)) = position
                && point.node == parent
                && point.offset == index
            {
                *point = RangeBoundary {
                    node: keeper,
                    offset: prefix,
                };
            }
        });
    }

    pub fn range_contents(&self, bounds: RangeBounds) -> Vec<RangeContent> {
        if bounds.collapsed() {
            return Vec::new();
        }
        let Some(common) = self.range_common_ancestor(bounds) else {
            return Vec::new();
        };
        if self.range_character_data(common).is_some() {
            return vec![RangeContent::Data(
                common,
                bounds.start.offset,
                bounds.end.offset,
            )];
        }
        self.get_node(common)
            .into_iter()
            .flat_map(|node| node.children.iter().copied())
            .filter_map(|id| self.range_content(id, bounds))
            .collect()
    }

    fn range_content(&self, id: NodeId, bounds: RangeBounds) -> Option<RangeContent> {
        let node = self.get_node(id)?;
        let parent = node.parent?;
        let index = self.get_node(parent)?.index_of_child(id)?;
        let before = RangeBoundary {
            node: parent,
            offset: index,
        };
        let after = RangeBoundary {
            node: parent,
            offset: index + 1,
        };
        if self.compare_range_boundaries(bounds.end, before)? != Ordering::Greater
            || self.compare_range_boundaries(bounds.start, after)? != Ordering::Less
        {
            return None;
        }
        if self.compare_range_boundaries(bounds.start, before)? != Ordering::Greater
            && self.compare_range_boundaries(bounds.end, after)? != Ordering::Less
        {
            return Some(RangeContent::Full(id));
        }
        if let Some(text) = self.range_character_data(id) {
            let start = if bounds.start.node == id {
                bounds.start.offset
            } else {
                0
            };
            let end = if bounds.end.node == id {
                bounds.end.offset
            } else {
                text.encode_utf16().count()
            };
            return Some(RangeContent::Data(id, start, end));
        }
        Some(RangeContent::Partial(
            id,
            node.children
                .iter()
                .filter_map(|id| self.range_content(*id, bounds))
                .collect(),
        ))
    }

    pub fn range_collapse_after_deletion(&self, bounds: RangeBounds) -> RangeBoundary {
        if self.range_contains(bounds.start.node, bounds.end.node) {
            return bounds.start;
        }
        let common = self
            .range_common_ancestor(bounds)
            .expect("range has one root");
        let mut child = bounds.start.node;
        while self.get_node(child).and_then(|node| node.parent) != Some(common) {
            child = self
                .get_node(child)
                .and_then(|node| node.parent)
                .expect("range ancestor");
        }
        RangeBoundary {
            node: common,
            offset: self
                .get_node(common)
                .unwrap()
                .index_of_child(child)
                .unwrap()
                + 1,
        }
    }

    pub fn range_text_parts(&self, bounds: RangeBounds) -> Vec<(NodeId, usize, usize)> {
        fn append(
            doc: &BaseDocument,
            content: &RangeContent,
            result: &mut Vec<(NodeId, usize, usize)>,
        ) {
            match content {
                RangeContent::Data(id, start, end) => {
                    if doc.get_node(*id).is_some_and(|node| node.is_text_node()) {
                        result.push((*id, *start, *end));
                    }
                }
                RangeContent::Partial(_, children) => {
                    for child in children {
                        append(doc, child, result);
                    }
                }
                RangeContent::Full(id) => {
                    let mut stack = vec![*id];
                    while let Some(id) = stack.pop() {
                        let Some(node) = doc.get_node(id) else {
                            continue;
                        };
                        if let NodeData::Text(text) = &node.data {
                            result.push((id, 0, text.content.encode_utf16().count()));
                        } else {
                            stack.extend(node.children.iter().rev().copied());
                        }
                    }
                }
            }
        }
        let mut result = Vec::new();
        for content in self.range_contents(bounds) {
            append(self, &content, &mut result);
        }
        result
    }

    pub fn range_string(&self, bounds: RangeBounds) -> String {
        let mut result = String::new();
        for (id, start, end) in self.range_text_parts(bounds) {
            if let Some(text) = self.range_character_data(id) {
                result.push_str(&utf16_slice(text, start, end));
            }
        }
        result
    }

    fn range_layout_extents(&self, root: NodeId) -> HashMap<NodeId, (usize, usize)> {
        use parley::PositionedLayoutItem;
        let mut result: HashMap<NodeId, (usize, usize)> = HashMap::new();
        let Some(inline) = self
            .get_node(root)
            .and_then(|node| node.element_data())
            .and_then(|element| element.inline_layout_data.as_ref())
        else {
            return result;
        };
        for line in inline.layout.lines() {
            for item in line.items() {
                if let PositionedLayoutItem::GlyphRun(run) = item
                    && let Some(id) = run.style().brush.text_node
                {
                    let range = run.run().text_range();
                    result
                        .entry(id)
                        .and_modify(|(start, end)| {
                            *start = (*start).min(range.start);
                            *end = (*end).max(range.end);
                        })
                        .or_insert((range.start, range.end));
                }
            }
        }
        result
    }

    /// Translate DOM text offsets into the inline layout's byte offsets.
    /// Extents are built once per inline root for this operation.
    pub fn range_layout_ranges(&self, bounds: RangeBounds) -> Vec<(NodeId, usize, usize)> {
        let mut extents = HashMap::new();
        let mut result: Vec<(NodeId, usize, usize)> = Vec::new();
        for (id, start, end) in self.range_text_parts(bounds) {
            let Some(root) = self
                .get_node(id)
                .filter(|node| node.flags.is_in_document())
                .and_then(|node| node.inline_root_ancestor())
                .map(|node| node.id)
            else {
                continue;
            };
            let map = extents
                .entry(root)
                .or_insert_with(|| self.range_layout_extents(root));
            let Some(&(base, limit)) = map.get(&id) else {
                continue;
            };
            let Some(text) = self.range_character_data(id) else {
                continue;
            };
            let start = (base + utf16_slice(text, 0, start).len()).min(limit);
            let end = (base + utf16_slice(text, 0, end).len()).min(limit);
            if start == end {
                continue;
            }
            if let Some((last_root, _, last_end)) = result.last_mut()
                && *last_root == root
                && *last_end == start
            {
                *last_end = end;
            } else {
                result.push((root, start, end));
            }
        }
        result
    }

    pub fn range_boundary_from_layout(
        &self,
        root: NodeId,
        offset: usize,
        end_boundary: bool,
    ) -> Option<RangeBoundary> {
        let extents = self.range_layout_extents(root);
        let mut candidates: Vec<_> = extents.into_iter().collect();
        candidates.sort_by_key(|(_, (start, _))| *start);
        for (id, (start, end)) in candidates {
            let matches = if end_boundary {
                offset > start && offset <= end
            } else {
                offset >= start && offset < end
            };
            if matches {
                let text = self.range_character_data(id)?;
                let mut bytes = offset.saturating_sub(start).min(text.len());
                while !text.is_char_boundary(bytes) {
                    bytes -= 1;
                }
                return Some(RangeBoundary {
                    node: id,
                    offset: text[..bytes].encode_utf16().count(),
                });
            }
        }
        None
    }

    pub fn range_client_rects(&self, bounds: RangeBounds) -> Vec<crate::kurbo::Rect> {
        use parley::{Affinity, Cursor, Selection};

        fn element_rects(
            doc: &BaseDocument,
            content: &RangeContent,
            result: &mut Vec<crate::kurbo::Rect>,
        ) {
            match content {
                RangeContent::Full(id)
                    if doc.get_node(*id).is_some_and(|node| node.is_element()) =>
                {
                    result.extend(doc.node_client_rects(*id).into_iter().map(|rect| {
                        crate::kurbo::Rect::new(
                            rect.x,
                            rect.y,
                            rect.x + rect.width,
                            rect.y + rect.height,
                        )
                    }));
                }
                RangeContent::Partial(_, children) => {
                    for child in children {
                        element_rects(doc, child, result);
                    }
                }
                _ => {}
            }
        }

        let mut result = Vec::new();
        for content in self.range_contents(bounds) {
            element_rects(self, &content, &mut result);
        }
        for (root_id, start, end) in self.range_layout_ranges(bounds) {
            let Some(root) = self.get_node(root_id) else {
                continue;
            };
            let Some(inline) = root
                .element_data()
                .and_then(|element| element.inline_layout_data.as_ref())
            else {
                continue;
            };
            let layout = &inline.layout;
            let scale = layout.scale() as f64;
            let box_layout = root.final_layout();
            let position = root.absolute_position(0.0, 0.0);
            let x = position.x as f64 + (box_layout.padding.left + box_layout.border.left) as f64
                - self.viewport_scroll.x;
            let y = position.y as f64 + (box_layout.padding.top + box_layout.border.top) as f64
                - self.viewport_scroll.y;
            let selection = Selection::new(
                Cursor::from_byte_index(layout, start, Affinity::Downstream),
                Cursor::from_byte_index(layout, end, Affinity::Downstream),
            );
            selection.geometry_with(layout, |rect, _| {
                let rect = self.transformed_client_rect(
                    root_id,
                    BoundingRect {
                        x: x + rect.x0 / scale,
                        y: y + rect.y0 / scale,
                        width: (rect.x1 - rect.x0) / scale,
                        height: (rect.y1 - rect.y0) / scale,
                    },
                );
                result.push(crate::kurbo::Rect::new(
                    rect.x,
                    rect.y,
                    rect.x + rect.width,
                    rect.y + rect.height,
                ));
            });
        }
        result
    }
}

pub(crate) type WeakRange = Weak<Mutex<RangeBounds>>;

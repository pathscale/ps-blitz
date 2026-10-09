//! Resolve out-of-flow boxes after their containing blocks have been sized.
//!
//! The first Taffy pass supplies normal flow and hypothetical static positions.
//! Only boxes whose containing block differs from their layout parent need a
//! second layout. Viewport-fixed boxes use the viewport rather than the root.

use std::collections::{HashMap, HashSet};

use blitz_traits::node_id::NodeId;
use style::properties::ComputedValues;
use style::properties::generated::longhands::position::computed_value::T as Position;
use style::selector_parser::RestyleDamage;
use style::values::computed::Rotate;
use style::values::generics::transform::{Scale, Translate};
use style::values::specified::box_::{DisplayInside, DisplayOutside};
use taffy::{Direction, Layout, Overflow, Point, ResolveOrZero, Size};
use thin_vec::ThinVec;

use super::{inline::layout_abspos_child, resolve_calc_value};
use crate::BaseDocument;

#[derive(Clone, Copy)]
pub(crate) struct AbsposPlacement {
    node_id: NodeId,
    containing_block: Option<NodeId>,
    offset: Point<f32>,
    auto_x: bool,
    auto_y: bool,
}

#[derive(Clone, Copy)]
pub(crate) struct AbsposCandidate {
    pub(crate) node_id: NodeId,
    pub(crate) containing_block: Option<NodeId>,
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) struct AbsposInputs {
    containing_block: Option<NodeId>,
    parent: NodeId,
    origin: Point<f32>,
    area: Size<f32>,
    parent_origin: Point<f32>,
    direction: Direction,
}

pub(crate) struct FixedLayoutParents {
    children: Vec<(NodeId, Option<ThinVec<NodeId>>)>,
    moved: Vec<(NodeId, NodeId)>,
}

pub(crate) fn establishes_transform_containing_block(styles: &ComputedValues) -> bool {
    let box_styles = styles.get_box();
    !box_styles.transform.0.is_empty()
        || !matches!(box_styles.translate, Translate::None)
        || !matches!(box_styles.rotate, Rotate::None)
        || !matches!(box_styles.scale, Scale::None)
}

impl BaseDocument {
    /// Restore authored child slots for the hypothetical-position pass without
    /// changing the paint tree assembled by the style flush.
    pub(crate) fn restore_fixed_layout_parents(&mut self) -> FixedLayoutParents {
        let root = self.root_element().id;
        let mut moved: Vec<_> = self
            .hoisted_fixed_parents
            .iter()
            .filter_map(|(&node_id, &parent)| {
                if !self.nodes.contains_key(node_id) || !self.nodes.contains_key(parent) {
                    return None;
                }
                let index = self.hoisted_fixed_indices.get(&node_id).copied()?;
                Some((node_id, parent, index))
            })
            .collect();
        moved.sort_by_key(|entry| entry.2);

        let mut saved = FixedLayoutParents {
            children: Vec::new(),
            moved: Vec::new(),
        };
        if moved.is_empty() {
            return saved;
        }

        saved.children.push((root, self.nodes[root].layout_children.borrow().clone()));
        for &(_, parent, _) in &moved {
            if !saved.children.iter().any(|entry| entry.0 == parent) {
                saved.children.push((parent, self.nodes[parent].layout_children.borrow().clone()));
            }
        }

        for (node_id, parent, index) in moved {
            if let Some(children) = self.nodes[root].layout_children.borrow_mut().as_mut() {
                children.retain(|id| *id != node_id);
            }
            {
                let mut children = self.nodes[parent].layout_children.borrow_mut();
                let children = children.get_or_insert_with(ThinVec::new);
                children.retain(|id| *id != node_id);
                children.insert(index.min(children.len()), node_id);
            }
            self.nodes[node_id].layout_parent.set(Some(parent));
            self.invalidate_abspos_ancestors(parent);
            saved.moved.push((node_id, parent));
        }
        saved
    }

    /// Return viewport-fixed nodes to their paint/layout host, translating
    /// locations instead of changing their document-space boxes.
    pub(crate) fn finish_fixed_layout_parents(&mut self, saved: FixedLayoutParents) {
        let root = self.root_element().id;
        let root_origin = self.abspos_layout_origin(root, false);
        let placements: Vec<_> = saved
            .moved
            .iter()
            .map(|&(node_id, parent)| {
                let parent_origin = self.abspos_layout_origin(parent, false);
                let location = self.nodes[node_id].unrounded_layout().location;
                (
                    node_id,
                    Point {
                        x: location.x + parent_origin.x - root_origin.x,
                        y: location.y + parent_origin.y - root_origin.y,
                    },
                )
            })
            .collect();

        for (parent, children) in saved.children {
            *self.nodes[parent].layout_children.borrow_mut() = children;
            self.invalidate_abspos_ancestors(parent);
        }
        for (node_id, location) in placements {
            self.nodes[node_id].layout_parent.set(Some(root));
            self.nodes[node_id].unrounded_layout_mut().location = location;
        }
    }

    /// Find the containing block through the box tree, including flattened
    /// positioned inline ancestors and the authored parent of a fixed box.
    pub(crate) fn abspos_containing_block(
        &self,
        node_id: NodeId,
        fixed: bool,
    ) -> Option<NodeId> {
        let mut current = node_id;
        loop {
            if let Some(id) = self.abspos_inline_ancestor(current, None, fixed) {
                return Some(id);
            }
            let node = self.nodes.get(current)?;
            let parent = if current == node_id
                && node.layout_parent.get() == Some(self.root_element().id)
            {
                self.hoisted_fixed_parents.get(&current).copied()
                    .or(node.layout_parent.get())
            } else {
                node.layout_parent.get()
            }?;
            let ancestor = self.nodes.get(parent)?;
            if ancestor.primary_styles().is_some_and(|styles| {
                establishes_transform_containing_block(&styles)
                    || (!fixed && styles.clone_position() != Position::Static)
            }) {
                return Some(parent);
            }
            current = parent;
        }
    }

    /// Zero-z-index boxes need paint hoisting only when normal painting would
    /// apply an overflow clip between the box and its containing block.
    pub(crate) fn abspos_escapes_clip(&self, node_id: NodeId) -> bool {
        let node = &self.nodes[node_id];
        let Some(styles) = node.primary_styles() else {
            return false;
        };
        let position = styles.clone_position();
        if !position.is_absolutely_positioned() {
            return false;
        }
        let cb = self.abspos_containing_block(node_id, position == Position::Fixed);
        let mut current = node.layout_parent.get();
        while let Some(id) = current {
            if Some(id) == cb {
                break;
            }
            let Some(ancestor) = self.nodes.get(id) else {
                break;
            };
            if ancestor.style_source_opt().is_some()
                && (ancestor.style().overflow.x != Overflow::Visible
                    || ancestor.style().overflow.y != Overflow::Visible)
            {
                return true;
            }
            current = ancestor.layout_parent.get();
        }
        false
    }

    pub(crate) fn resolve_abspos_layout(
        &mut self,
        _root: NodeId,
        viewport: Size<f32>,
    ) -> Vec<AbsposPlacement> {
        let mut previous: HashMap<_, _> = std::mem::take(&mut self.abspos_placements)
            .into_iter()
            .map(|placement| (placement.node_id, placement))
            .collect();
        let mut placements = Vec::new();
        let mut changed_ancestors = HashSet::new();

        // The existing fixed/sticky walk maintains this list in preorder.
        // An outer correction can therefore damage a later nested candidate.
        for index in 0..self.abspos_candidates.len() {
            let candidate = self.abspos_candidates[index];
            let node_id = candidate.node_id;
            let Some(node) = self.nodes.get(node_id) else {
                continue;
            };
            if node.is_display_none() || node.style_source_opt().is_none() {
                continue;
            }
            let Some(parent) = node.layout_parent.get() else {
                continue;
            };
            let cb = candidate.containing_block;
            if cb.is_some_and(|id| !self.nodes.contains_key(id)) {
                continue;
            }
            if cb == Some(parent) {
                continue;
            }

            let (position, inline_level) = node.primary_styles()
                .map(|styles| {
                    (
                        styles.clone_position(),
                        styles.get_box().original_display.outside() == DisplayOutside::Inline,
                    )
                })
                .unwrap_or((Position::Static, false));
            if !position.is_absolutely_positioned() {
                continue;
            }

            let (origin, area) = cb
                .map(|id| self.abspos_padding_box(id, false))
                .unwrap_or((Point::ZERO, viewport));
            let parent_origin = self.abspos_layout_origin(parent, false);
            let direction = self.nodes[parent].style().direction;
            let inputs = AbsposInputs {
                containing_block: cb,
                parent,
                origin,
                area,
                parent_origin,
                direction,
            };

            if self.incremental_layout
                && !self.abspos_written.contains(&node_id)
                && self.abspos_inputs.get(&node_id) == Some(&inputs)
            {
                if let Some(placement) = previous.remove(&node_id) {
                    placements.push(placement);
                }
                continue;
            }

            let old: Layout = *self.nodes[node_id].unrounded_layout();
            let style = self.nodes[node_id].style();
            let auto_x = style.inset.left.is_auto() && style.inset.right.is_auto();
            let auto_y = style.inset.top.is_auto() && style.inset.bottom.is_auto();

            // PerformLayout cache entries own descendant side effects. Clear
            // only the damaged candidate subtree before changing its inputs.
            self.release_abspos_subtree(node_id);
            layout_abspos_child(
                self,
                node_id.as_u64(),
                old.location,
                inline_level,
                area,
                Point {
                    x: origin.x - parent_origin.x,
                    y: origin.y - parent_origin.y,
                },
                direction,
            );
            let layout = self.nodes[node_id].unrounded_layout_mut();
            layout.order = old.order;

            // Preserve the hypothetical static anchor independently on
            // each auto-inset axis. Percentage margins use the new CB.
            if auto_x {
                layout.location.x = if inline_level && direction == Direction::Rtl {
                    old.location.x + old.size.width + old.margin.right
                        - layout.size.width - layout.margin.right
                } else {
                    old.location.x - old.margin.left + layout.margin.left
                };
            }
            if auto_y {
                layout.location.y = old.location.y - old.margin.top + layout.margin.top;
            }
            self.release_abspos_subtree(node_id);
            self.abspos_inputs.insert(node_id, inputs);

            // Keep ancestor caches: their retained descendant side effects
            // now contain the corrected boxes. Clearing them would force the
            // same hypothetical pass on the next unchanged resolve.
            let mut current = Some(node_id);
            while let Some(id) = current {
                if !changed_ancestors.insert(id) {
                    break;
                }
                current = self.nodes[id].layout_parent.get();
            }

            // Viewport-fixed nodes use the existing viewport-scroll pin
            // after they are returned to the root. Other escaped boxes
            // need their intermediate ancestors' scrolling cancelled.
            if position != Position::Fixed || cb.is_some() {
                placements.push(AbsposPlacement {
                    node_id,
                    containing_block: cb,
                    offset: Point::ZERO,
                    auto_x,
                    auto_y,
                });
            }
        }

        // Rebuild only paths affected by a correction, deepest first.
        let mut changed: Vec<_> = changed_ancestors.into_iter()
            .map(|id| {
                let mut depth = 0;
                let mut current = self.nodes[id].layout_parent.get();
                while let Some(parent) = current {
                    depth += 1;
                    current = self.nodes[parent].layout_parent.get();
                }
                (depth, id)
            })
            .collect();
        changed.sort_unstable_by(|left, right| right.0.cmp(&left.0));
        for (_, id) in changed {
            self.rebuild_abspos_content_size(id);
            self.nodes[id].insert_damage(RestyleDamage::RECALCULATE_OVERFLOW);
        }
        self.abspos_written.clear();
        placements
    }

    /// Non-atomic positioned inline ancestors are flattened out of the layout
    /// tree. Search the intervening DOM chain before accepting the box-tree CB.
    fn abspos_inline_ancestor(
        &self,
        node_id: NodeId,
        fallback: Option<NodeId>,
        fixed: bool,
    ) -> Option<NodeId> {
        let node = &self.nodes[node_id];
        let stop = node.layout_parent.get();
        let mut ancestor = node.parent;
        while let Some(id) = ancestor {
            if Some(id) == stop {
                break;
            }
            let Some(node) = self.nodes.get(id) else {
                break;
            };
            if let Some(styles) = node.primary_styles() {
                let display = styles.clone_display();
                if display.outside() == DisplayOutside::Inline
                    && display.inside() == DisplayInside::Flow
                    && (establishes_transform_containing_block(&styles)
                        || (!fixed && styles.clone_position() != Position::Static))
                {
                    return Some(id);
                }
            }
            ancestor = node.parent;
        }
        fallback
    }

    fn abspos_layout_origin(&self, node_id: NodeId, rounded: bool) -> Point<f32> {
        let mut origin = Point::ZERO;
        let mut current = Some(node_id);
        while let Some(id) = current {
            let Some(node) = self.nodes.get(id) else {
                break;
            };
            let layout = if rounded { node.final_layout() } else { node.unrounded_layout() };
            origin.x += layout.location.x;
            origin.y += layout.location.y;
            current = node.layout_parent.get();
        }
        origin
    }

    fn abspos_padding_box(&self, node_id: NodeId, rounded: bool) -> (Point<f32>, Size<f32>) {
        if let Some(area) = self.abspos_inline_padding_box(node_id, rounded) {
            return area;
        }
        let node = &self.nodes[node_id];
        let layout = if rounded { node.final_layout() } else { node.unrounded_layout() };
        let origin = self.abspos_layout_origin(node_id, rounded);
        (
            Point {
                x: origin.x + layout.border.left,
                y: origin.y + layout.border.top,
            },
            Size {
                width: (layout.size.width - layout.border.left - layout.border.right
                    - layout.scrollbar_size.width).max(0.0),
                height: (layout.size.height - layout.border.top - layout.border.bottom
                    - layout.scrollbar_size.height).max(0.0),
            },
        )
    }

    fn abspos_inline_padding_box(
        &self,
        node_id: NodeId,
        rounded: bool,
    ) -> Option<(Point<f32>, Size<f32>)> {
        let node = self.nodes.get(node_id)?;
        let styles = node.primary_styles()?;
        let display = styles.clone_display();
        if node.flags.is_inline_root()
            || display.outside() != DisplayOutside::Inline
            || display.inside() != DisplayInside::Flow
        {
            return None;
        }
        let root = node.inline_root_ancestor()?;
        let inline = root.element_data()?.inline_layout_data.as_ref()?;
        let scale = inline.layout.scale();

        let in_target = |mut id: NodeId| {
            loop {
                if id == node_id {
                    return true;
                }
                if id == root.id {
                    return false;
                }
                let Some(parent) = self.nodes.get(id).and_then(|node| node.parent) else {
                    return false;
                };
                id = parent;
            }
        };

        let mut first: Option<(f32, f32, f32, f32)> = None;
        let mut last = None;
        for line in inline.layout.lines() {
            let mut bounds: Option<(f32, f32, f32, f32)> = None;
            for item in line.items() {
                let rect = match item {
                    parley::PositionedLayoutItem::GlyphRun(run)
                        if in_target(run.style().brush.id) =>
                    {
                        let metrics = line.metrics();
                        Some((
                            run.offset(), metrics.block_min_coord,
                            run.offset() + run.advance(), metrics.block_max_coord,
                        ))
                    }
                    parley::PositionedLayoutItem::InlineBox(ibox) => {
                        let id = NodeId::from_u64(ibox.id);
                        if in_target(id)
                            && self.nodes[id].style().position != taffy::Position::Absolute
                        {
                            Some((ibox.x, ibox.y, ibox.x + ibox.width, ibox.y + ibox.height))
                        } else {
                            None
                        }
                    }
                    _ => None,
                };
                if let Some((x0, y0, x1, y1)) = rect {
                    bounds = Some(match bounds {
                        Some((a, b, c, d)) => (a.min(x0), b.min(y0), c.max(x1), d.max(y1)),
                        None => (x0, y0, x1, y1),
                    });
                }
            }
            if let Some(bounds) = bounds {
                first.get_or_insert(bounds);
                last = Some(bounds);
            }
        }

        let root_layout = if rounded { root.final_layout() } else { root.unrounded_layout() };
        let root_origin = self.abspos_layout_origin(root.id, rounded);
        let style = stylo_taffy::to_taffy_style(&styles);
        let padding = style.padding.resolve_or_zero(
            Some(root_layout.content_box_width()), resolve_calc_value,
        );
        let first = first.unwrap_or((0.0, 0.0, 0.0, 0.0));
        let last = last.unwrap_or(first);
        let x = first.0 / scale - padding.left;
        let y = first.1 / scale - padding.top;
        Some((
            Point {
                x: root_origin.x + root_layout.border.left + root_layout.padding.left + x,
                y: root_origin.y + root_layout.border.top + root_layout.padding.top + y,
            },
            Size {
                width: (last.2 / scale + padding.right - x).max(0.0),
                height: (last.3 / scale + padding.bottom - y).max(0.0),
            },
        ))
    }

    fn release_abspos_subtree(&mut self, node_id: NodeId) {
        let Some(node) = self.nodes.get_mut(node_id) else {
            return;
        };
        node.cache_release();
        node.insert_damage(RestyleDamage::RECALCULATE_OVERFLOW);
        if let Some(inline) = node.data.downcast_element_mut()
            .and_then(|element| element.inline_layout_data.as_mut())
        {
            inline.content_widths = None;
        }
        let children = node.layout_children.borrow().clone();
        if let Some(children) = children {
            for child in children {
                self.release_abspos_subtree(child);
            }
        }
    }

    fn invalidate_abspos_ancestors(&mut self, node_id: NodeId) {
        let mut current = Some(node_id);
        while let Some(id) = current {
            let Some(node) = self.nodes.get_mut(id) else {
                break;
            };
            node.cache_release();
            node.insert_damage(RestyleDamage::RECALCULATE_OVERFLOW);
            current = node.layout_parent.get();
        }
    }

    /// Rebuild extents rather than unioning new boxes with obsolete abspos
    /// extents, so a smaller containing block can also shrink a scroll range.
    fn rebuild_abspos_content_size(&mut self, node_id: NodeId) {
        let node = &self.nodes[node_id];
        let layout = *node.unrounded_layout();
        let mut content = Size {
            width: (layout.size.width - layout.border.left - layout.border.right
                - layout.scrollbar_size.width).max(0.0),
            height: (layout.size.height - layout.border.top - layout.border.bottom
                - layout.scrollbar_size.height).max(0.0),
        };
        if let Some(inline) = node.element_data()
            .and_then(|element| element.inline_layout_data.as_ref())
        {
            let scale = inline.layout.scale();
            content.width = content.width.max(
                inline.layout.width() / scale + layout.padding.left + layout.padding.right,
            );
            content.height = content.height.max(
                inline.layout.height() / scale + layout.padding.top + layout.padding.bottom,
            );
        }
        if let Some(children) = node.layout_children.borrow().as_ref() {
            for &id in children {
                let child = &self.nodes[id];
                if child.is_display_none()
                    || child.primary_styles().is_some_and(|styles| styles.clone_position() == Position::Fixed)
                {
                    continue;
                }
                let child_layout = child.unrounded_layout();
                let overflow = child.style().overflow;
                let width = if overflow.x == Overflow::Visible {
                    child_layout.size.width.max(child_layout.content_size.width + child_layout.border.left)
                } else {
                    child_layout.size.width
                };
                let height = if overflow.y == Overflow::Visible {
                    child_layout.size.height.max(child_layout.content_size.height + child_layout.border.top)
                } else {
                    child_layout.size.height
                };
                content.width = content.width.max(
                    child_layout.location.x + width + child_layout.margin.right
                        + layout.padding.right - layout.border.left,
                );
                content.height = content.height.max(
                    child_layout.location.y + height + child_layout.margin.bottom
                        + layout.padding.bottom - layout.border.top,
                );
            }
        }
        self.nodes[node_id].unrounded_layout_mut().content_size = content;
    }

    pub(crate) fn finish_abspos_placements(&mut self, mut placements: Vec<AbsposPlacement>) {
        for placement in &mut placements {
            let origin = self.abspos_layout_origin(placement.node_id, true);
            let cb_origin = placement.containing_block
                .map(|id| self.abspos_padding_box(id, true).0)
                .unwrap_or(Point::ZERO);
            placement.offset = Point {
                x: origin.x - cb_origin.x,
                y: origin.y - cb_origin.y,
            };
        }
        self.abspos_placements = placements;
    }

    /// Keep explicit-inset axes attached to their CB while intermediate static
    /// ancestors scroll or move under sticky positioning. Auto axes retain
    /// their local static positions.
    pub(crate) fn resolve_abspos_positions(&mut self) {
        for index in 0..self.abspos_placements.len() {
            let placement = self.abspos_placements[index];
            let Some(node) = self.nodes.get(placement.node_id) else {
                continue;
            };
            let Some(parent_id) = node.layout_parent.get() else {
                continue;
            };
            let parent = &self.nodes[parent_id];
            let parent_origin = parent.absolute_position(0.0, 0.0);
            let parent_scroll = *parent.scroll_offset();
            let cb_origin = match placement.containing_block {
                Some(id) if self.nodes.contains_key(id) => {
                    let cb = &self.nodes[id];
                    let origin = cb.absolute_position(0.0, 0.0);
                    let box_origin = self.abspos_padding_box(id, true).0;
                    let layout_origin = self.abspos_layout_origin(id, true);
                    Point {
                        x: origin.x + box_origin.x - layout_origin.x - cb.scroll_offset().x as f32,
                        y: origin.y + box_origin.y - layout_origin.y - cb.scroll_offset().y as f32,
                    }
                }
                Some(_) => continue,
                None => Point::ZERO,
            };
            let old = self.nodes[placement.node_id].final_layout().location;
            let location = Point {
                x: if placement.auto_x { old.x } else {
                    cb_origin.x + placement.offset.x - parent_origin.x + parent_scroll.x as f32
                },
                y: if placement.auto_y { old.y } else {
                    cb_origin.y + placement.offset.y - parent_origin.y + parent_scroll.y as f32
                },
            };
            if location != old {
                self.nodes[placement.node_id].final_layout_mut().location = location;
                let mut current = Some(placement.node_id);
                while let Some(id) = current {
                    let Some(node) = self.nodes.get_mut(id) else {
                        break;
                    };
                    node.insert_damage(RestyleDamage::RECALCULATE_OVERFLOW);
                    current = node.layout_parent.get();
                }
            }
        }
    }
}

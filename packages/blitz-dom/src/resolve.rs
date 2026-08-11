//! Resolve style and layout

use blitz_traits::node_id::NodeId;
use std::{
    cell::RefCell,
    time::{SystemTime, UNIX_EPOCH},
};

use debug_timer::debug_timer;
use kurbo::{Affine, Rect};
use parley::LayoutContext;
use selectors::Element as _;
use style::dom::TDocument;

#[cfg(feature = "parallel-construct")]
use rayon::prelude::*;

// FIXME: static thread_local FontCtx isn't necessarily correct in multi-document context.
// Should use thread_local crate with ThreadLocal value store in the Document.
thread_local! {
    pub(crate) static LAYOUT_CTX: RefCell<Option<Box<LayoutContext<TextBrush>>>> = const { RefCell::new(None) };
}

use style::properties::ComputedValues;
use style::properties::generated::longhands::position::computed_value::T as Position;
use style::selector_parser::RestyleDamage;
use style::values::computed::Rotate;
use style::values::generics::transform::{Scale, Translate};
use taffy::AvailableSpace;

use crate::{
    BaseDocument,
    events::ScrollAnimationState,
    layout::{
        construct::{
            ConstructionTask, ConstructionTaskData, ConstructionTaskResult,
            ConstructionTaskResultData, LayoutChildren, build_inline_layout_into,
            collect_layout_children,
        },
        damage::{ALL_DAMAGE, CONSTRUCT_BOX, CONSTRUCT_DESCENDENT, CONSTRUCT_FC},
    },
    node::TextBrush,
};

impl BaseDocument {
    /// Restyle the tree and then relayout it
    pub fn resolve(&mut self, current_time_for_animations: f64) {
        if TDocument::as_node(&self.root_node())
            .first_element_child()
            .is_none()
        {
            #[cfg(feature = "tracing")]
            tracing::warn!("No DOM - not resolving");
            return;
        }

        // Process messages that have been sent to our message channel (e.g. loaded resource)
        self.handle_messages();

        self.resolve_scroll_animation();

        // Retain completed activity entries so an initially visible scrollbar
        // stays faded after its first interaction. Only removed nodes need to
        // shed their entry.
        let nodes = &self.nodes;
        self.scrollbar_activity
            .retain(|node_id, _| nodes.contains_key(*node_id));

        let root_node_id = self.root_element().id;
        debug_timer!(timer, feature = "log-phase-times");

        // Compute the shadow DOM flattened tree (shadow-root composition and
        // <slot> distribution). This must happen *before* style resolution so
        // that Stylo traverses the composed (flattened) tree and styles shadow
        // content, and before box construction consumes it.
        #[cfg(feature = "shadow-dom")]
        {
            self.compute_flattened_trees();
            timer.record_time("shadow");
        }

        // we need to resolve stylist first since it will need to drive our layout bits
        self.resolve_stylist(current_time_for_animations);
        timer.record_time("style");

        // Propagate damage flags (from mutation and restyles) up and down the tree
        if self.incremental_layout {
            self.propagate_damage_flags(root_node_id, RestyleDamage::empty());
            timer.record_time("damage");
        }

        // Fix up tree for layout (insert anonymous blocks as necessary, etc)
        self.resolve_layout_children();
        timer.record_time("construct");

        self.resolve_deferred_tasks();
        timer.record_time("pconstruct");

        self.hoist_fixed_position_nodes();
        timer.record_time("hoist");

        // Merge stylo into taffy
        self.flush_styles_to_layout(root_node_id);
        timer.record_time("flush");

        // Next we resolve layout with the data resolved by stlist
        self.resolve_layout();
        self.correct_hoisted_fixed_positions();
        timer.record_time("layout");

        // Resolve transforms
        self.resolve_transforms(root_node_id);
        timer.record_time("transform");

        // Clear all damage and dirty flags
        if self.incremental_layout {
            for (_, node) in self.nodes.iter_mut() {
                node.clear_damage_mut();
                node.unset_dirty_descendants();
            }
            timer.record_time("c_damage");
        }

        // Re-resolve the hover node from the pointer position against the fresh
        // layout. This must run *after* the damage/dirty flags are cleared
        // above, so that the restyle hint and ancestor `dirty_descendants`
        // flags set by any resulting hover change survive into the next resolve
        // pass (the clearing loop would otherwise wipe them). Any resulting
        // restyle is picked up on the next resolve pass; a redraw is requested
        // if the hovered node actually changes.
        self.refresh_hover();

        let mut subdoc_is_animating = false;
        for &node_id in &self.sub_document_nodes {
            let node = &mut self.nodes[node_id];
            let size = node.final_layout().size;
            if let Some(mut sub_doc) = node.subdoc_mut().map(|doc| doc.inner_mut()) {
                // Set viewport
                // viewport_mut handles change detection. So we just unconditionally set the values;
                let mut sub_viewport = sub_doc.viewport_mut();
                sub_viewport.hidpi_scale = self.viewport.hidpi_scale;
                sub_viewport.zoom = self.viewport.zoom;
                sub_viewport.color_scheme = self.viewport.color_scheme;

                let viewport_scale = self.viewport.scale();
                sub_viewport.window_size = (
                    (size.width * viewport_scale) as u32,
                    (size.height * viewport_scale) as u32,
                );
                drop(sub_viewport);

                sub_doc.resolve(current_time_for_animations);

                subdoc_is_animating |= sub_doc.is_animating();
            }
        }
        self.subdoc_is_animating = subdoc_is_animating;
        timer.record_time("subdocs");

        // Printed with the phases so a single line says both how long layout
        // took and how much of the tree it touched. Without the counts the
        // timings cannot distinguish a few slow nodes from a cache miss across
        // the document, and those need opposite fixes.
        #[cfg(feature = "log-phase-times")]
        {
            // Named before the counters are drained, and only when the pass was
            // expensive enough to be worth looking at.
            let offenders = crate::layout::layout_counters::worst_offenders(6);
            if offenders.first().is_some_and(|(_, count)| *count > 8) {
                let described: Vec<String> = offenders
                    .iter()
                    .map(|(id, count)| {
                        let tag = self
                            .nodes
                            .get(*id)
                            .and_then(|node| node.element_data())
                            .map(|element| element.name.local.to_string())
                            .unwrap_or_else(|| "?".to_string());
                        let display = self
                            .nodes
                            .get(*id)
                            .map(|node| format!("{:?}", node.style.display))
                            .unwrap_or_default();
                        format!("{id}:{tag}({display})x{count}")
                    })
                    .collect();
                println!("  layout hotspots: {}", described.join(" "));
            }
            let counts = crate::layout::layout_counters::take();
            let total_nodes = self.nodes.len();
            let hit_rate = if counts.lookups > 0 {
                (counts.hits as f64 / counts.lookups as f64) * 100.0
            } else {
                0.0
            };
            timer.print_times(&format!(
                "Resolve({}) [computed {} over {} distinct of {total_nodes} nodes, \
                 cache {}/{} hits {hit_rate:.0}%, {} cleared]: ",
                self.id(),
                counts.computed,
                counts.distinct,
                counts.hits,
                counts.lookups,
                counts.caches_cleared,
            ));
        }
        #[cfg(not(feature = "log-phase-times"))]
        timer.print_times(&format!("Resolve({}): ", self.id()));
    }

    fn resolve_transforms(&mut self, node_id: NodeId) -> Rect {
        if !self.nodes.contains_key(node_id) {
            return Rect::ZERO;
        }

        if !self.nodes[node_id]
            .damage()
            .map(|d| d.contains(style::selector_parser::RestyleDamage::RECALCULATE_OVERFLOW))
            .unwrap_or(false)
        {
            return *self.nodes[node_id].scrollable_overflow();
        }

        let scale = self.viewport.scale_f64();

        let transform = self.nodes[node_id].set_transform(scale as f32);

        let w = self.nodes[node_id].final_layout().size.width as f64 * scale;
        let h = self.nodes[node_id].final_layout().size.height as f64 * scale;
        let mut overflow = Rect::new(0.0, 0.0, w, h);

        let layout_children = std::mem::take(self.nodes[node_id].layout_children.get_mut());

        if let Some(ref children) = layout_children {
            for &child_id in children {
                let child_rect_in_self = self.resolve_transforms(child_id);
                overflow = overflow.union(child_rect_in_self);
            }
        }
        if let Some(before) = self.nodes[node_id].before() {
            let child_rect_in_self = self.resolve_transforms(before);
            overflow = overflow.union(child_rect_in_self);
        }
        if let Some(after) = self.nodes[node_id].after() {
            let child_rect_in_self = self.resolve_transforms(after);
            overflow = overflow.union(child_rect_in_self);
        }

        *self.nodes[node_id].scrollable_overflow_mut() = overflow;
        *self.nodes[node_id].layout_children.get_mut() = layout_children;

        let scaled_x = self.nodes[node_id].final_layout().location.x as f64 * scale;
        let scaled_y = self.nodes[node_id].final_layout().location.y as f64 * scale;

        let full = if let Some(t) = transform {
            Affine::translate((scaled_x, scaled_y)) * t
        } else {
            Affine::translate((scaled_x, scaled_y))
        };

        full.transform_rect_bbox(overflow)
    }

    pub fn resolve_scroll_animation(&mut self) {
        match &mut self.scroll_animation {
            ScrollAnimationState::Fling(fling_state) => {
                let time_ms = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_millis() as u64 as f64;

                let time_diff_ms = time_ms - fling_state.last_seen_time;

                // 0.95 @ 60fps normalized to actual frame times
                let deceleration = 1.0 - ((0.05 / 16.66666) * time_diff_ms);

                fling_state.x_velocity *= deceleration;
                fling_state.y_velocity *= deceleration;
                fling_state.last_seen_time = time_ms;
                let fling_state = fling_state.clone();

                let dx = fling_state.x_velocity * time_diff_ms;
                let dy = fling_state.y_velocity * time_diff_ms;

                self.scroll_by(Some(fling_state.target), dx, dy, &mut |_| {});
                if fling_state.x_velocity.abs() < 0.1 && fling_state.y_velocity.abs() < 0.1 {
                    self.scroll_animation = ScrollAnimationState::None;
                }
            }
            ScrollAnimationState::None => {
                // Do nothing
            }
        }
    }

    /// Ensure that the layout_children field is populated for all nodes
    pub fn resolve_layout_children(&mut self) {
        resolve_layout_children_recursive(self, self.root_node().id);

        fn resolve_layout_children_recursive(doc: &mut BaseDocument, node_id: NodeId) {
            // Anonymous blocks and pseudo-elements can be removed from the slab
            // between render passes. Bail out rather than panicking on a stale key.
            if doc.nodes.get(node_id).is_none() {
                return;
            }

            let mut damage = doc.nodes[node_id].damage().unwrap_or(ALL_DAMAGE);
            let _flags = doc.nodes[node_id].flags;

            if !doc.incremental_layout || damage.intersects(CONSTRUCT_FC | CONSTRUCT_BOX) {
                //} || flags.contains(NodeFlags::IS_INLINE_ROOT) {

                // Deallocate the anonymous blocks created for this node in the
                // previous construction round. They live only in the slab, so
                // reconstructing without freeing them would leak a slab entry per
                // anonymous block per reconstruction.
                let old_anonymous_blocks = std::mem::take(&mut doc.nodes[node_id].anonymous_blocks);
                for anon_id in old_anonymous_blocks {
                    doc.deallocate_anonymous_block(anon_id);
                }

                let mut collected = LayoutChildren::default();
                collect_layout_children(doc, node_id, &mut collected);
                let layout_children = collected.children;
                doc.nodes[node_id].anonymous_blocks = collected.anonymous_blocks;

                // Recurse into newly collected layout children
                for child_id in layout_children.iter().copied() {
                    resolve_layout_children_recursive(doc, child_id);
                    doc.nodes[child_id].layout_parent.set(Some(node_id));
                    if let Some(mut data) = doc.nodes[child_id]
                        .stylo_element_data_opt_mut()
                        .and_then(|s| s.get_mut())
                    {
                        data.damage
                            .remove(CONSTRUCT_DESCENDENT | CONSTRUCT_FC | CONSTRUCT_BOX);
                    }
                }

                *doc.nodes[node_id].layout_children.borrow_mut() = Some(layout_children.clone());
                // *doc.nodes[node_id].paint_children.borrow_mut() = Some(layout_children);

                damage.remove(CONSTRUCT_DESCENDENT | CONSTRUCT_FC | CONSTRUCT_BOX);
                // damage.insert(RestyleDamage::RELAYOUT | RestyleDamage::REPAINT);
            } else {
                //if damage.contains(CONSTRUCT_DESCENDENT) {
                let layout_children = doc.nodes[node_id].layout_children.borrow_mut().take();
                if let Some(layout_children) = layout_children {
                    for child_id in layout_children.iter().copied() {
                        // Anonymous blocks and pseudo-elements can be removed from the
                        // slab between render passes; skip stale IDs.
                        if !doc.nodes.contains_key(child_id) {
                            continue;
                        }
                        resolve_layout_children_recursive(doc, child_id);
                        doc.nodes[child_id].layout_parent.set(Some(node_id));
                    }

                    *doc.nodes[node_id].layout_children.borrow_mut() = Some(layout_children);
                }

                // damage.remove(CONSTRUCT_DESCENDENT);
                // damage.insert(RestyleDamage::RELAYOUT | RestyleDamage::REPAINT);
            }

            doc.nodes[node_id].set_damage(damage);
        }
    }

    /// Reparent `position: fixed` nodes onto the root element for layout.
    ///
    /// Taffy has no `Fixed` position, so `stylo_taffy` maps it to `Absolute`. An
    /// absolutely positioned node resolves its insets against its containing
    /// block, which for a fixed node must be the viewport. Laid out in place it
    /// would instead resolve against the nearest positioned ancestor, so both its
    /// offset and — when opposite insets are set — its size come out wrong.
    ///
    /// Reparenting them onto the root element takes the positioned ancestor out
    /// of the picture. This runs after `resolve_layout_children` and before
    /// `flush_styles_to_layout`, which derives `paint_children` from
    /// `layout_children`, so painting and hit testing follow the hoist without
    /// further work.
    ///
    /// Note this is not yet the full containing block a browser would use. The
    /// root element takes its height from its content, whereas the initial
    /// containing block is always viewport-sized, so `inset: 0` still sizes
    /// against the document rather than the viewport. Closing that gap needs an
    /// ICB distinct from the root element.
    ///
    /// A transformed ancestor becomes the containing block for its fixed
    /// descendants, so those are left where they are.
    ///
    /// <https://drafts.csswg.org/css-position/#fixed-pos>
    /// <https://drafts.csswg.org/css-transforms-1/#propdef-transform>
    pub fn hoist_fixed_position_nodes(&mut self) {
        let root_id = self.root_element().id;

        let mut hoisted: Vec<NodeId> = Vec::new();
        collect_fixed(self, root_id, false, &mut hoisted);

        // Rebuilt every pass: the tree may have changed, and a node that is no
        // longer fixed must stop being attributed to an old parent.
        self.hoisted_fixed_parents.clear();

        for node_id in hoisted {
            let Some(parent_id) = self.nodes[node_id].layout_parent.get() else {
                continue;
            };
            if parent_id == root_id {
                continue;
            }

            // Remember where it came from. The hoist decides the containing
            // block; the box tree still decides the stacking context, and
            // `flush_styles_to_layout` reads this to keep them apart.
            self.hoisted_fixed_parents.insert(node_id, parent_id);

            if let Some(children) = self.nodes[parent_id].layout_children.borrow_mut().as_mut() {
                children.retain(|id| *id != node_id);
            }
            if let Some(children) = self.nodes[root_id].layout_children.borrow_mut().as_mut() {
                children.push(node_id);
            }
            self.nodes[node_id].layout_parent.set(Some(root_id));
        }

        fn collect_fixed(
            doc: &BaseDocument,
            node_id: NodeId,
            under_transform: bool,
            out: &mut Vec<NodeId>,
        ) {
            let children = doc.nodes[node_id].layout_children.borrow().clone();
            let Some(children) = children else {
                return;
            };

            for child_id in children {
                let Some(child) = doc.nodes.get(child_id) else {
                    continue;
                };
                let Some(styles) = child.primary_styles() else {
                    continue;
                };

                if !under_transform && styles.clone_position() == Position::Fixed {
                    out.push(child_id);
                }

                collect_fixed(
                    doc,
                    child_id,
                    under_transform || establishes_containing_block(&styles),
                    out,
                );
            }
        }

        /// Whether a node becomes the containing block for fixed descendants.
        ///
        /// TODO: `filter`, `backdrop-filter`, `will-change`, `contain` and
        /// `perspective` also do this.
        fn establishes_containing_block(styles: &ComputedValues) -> bool {
            let box_styles = styles.get_box();
            !box_styles.transform.0.is_empty()
                || !matches!(box_styles.translate, Translate::None)
                || !matches!(box_styles.rotate, Rotate::None)
                || !matches!(box_styles.scale, Scale::None)
        }
    }

    /// Give each held fixed layer the offset that cancels its hoist.
    ///
    /// Paint draws a hoisted child at its stacking context root's origin, plus
    /// the recorded offset, plus the node's own layout location — and that
    /// location is relative to the root element, because the hoist made the
    /// root its layout parent. So the offset has to carry the difference
    /// between the two origins, or a background mounted with `inset: 0` lands
    /// wherever its isolate happens to sit rather than over the viewport.
    ///
    /// Separate from `flush_styles_to_layout`, which decides *which* context
    /// holds the layer: that runs before taffy, when every absolute position is
    /// still zero.
    pub(crate) fn correct_hoisted_fixed_positions(&mut self) {
        if self.hoisted_fixed_parents.is_empty() {
            return;
        }
        let root_id = self.root_element().id;
        let root_abs = self.nodes[root_id].absolute_position(0.0, 0.0);

        let placements: Vec<(NodeId, NodeId)> = self
            .hoisted_fixed_parents
            .iter()
            .filter_map(|(&node_id, &origin)| {
                let host = self.nearest_stacking_context_ancestor(origin)?;
                (host != root_id).then_some((node_id, host))
            })
            .collect();

        for (node_id, host) in placements {
            let host_abs = self.nodes[host].absolute_position(0.0, 0.0);
            let Some(context) = self.nodes[host].stacking_context.as_mut() else {
                continue;
            };
            for child in context.children.iter_mut() {
                if child.node_id == node_id {
                    child.position = taffy::Point {
                        x: root_abs.x - host_abs.x,
                        y: root_abs.y - host_abs.y,
                    };
                }
            }
        }
    }

    pub fn resolve_deferred_tasks(&mut self) {
        let mut deferred_construction_nodes = std::mem::take(&mut self.deferred_construction_nodes);

        // Deduplicate deferred tasks by node_id to avoid redundant work
        deferred_construction_nodes.sort_unstable_by_key(|task| task.node_id);
        deferred_construction_nodes.dedup_by_key(|task| task.node_id);

        #[cfg(feature = "parallel-construct")]
        let iter = deferred_construction_nodes.into_par_iter();
        #[cfg(not(feature = "parallel-construct"))]
        let iter = deferred_construction_nodes.into_iter();

        let results: Vec<ConstructionTaskResult> = iter
            .map(|task: ConstructionTask| match task.data {
                ConstructionTaskData::InlineLayout(mut layout) => {
                    #[cfg(feature = "parallel-construct")]
                    let mut layout_ctx = LAYOUT_CTX
                        .take()
                        .unwrap_or_else(|| Box::new(LayoutContext::new()));
                    #[cfg(feature = "parallel-construct")]
                    let layout_ctx_mut = &mut layout_ctx;

                    #[cfg(feature = "parallel-construct")]
                    let mut font_ctx = self
                        .thread_font_contexts
                        .get_or(|| RefCell::new(Box::new(self.font_ctx.lock().unwrap().clone())))
                        .borrow_mut();
                    #[cfg(feature = "parallel-construct")]
                    let font_ctx_mut = &mut *font_ctx;

                    #[cfg(not(feature = "parallel-construct"))]
                    let layout_ctx_mut = &mut self.layout_ctx;
                    #[cfg(not(feature = "parallel-construct"))]
                    let font_ctx_mut = &mut *self.font_ctx.lock().unwrap();

                    layout.content_widths = None;
                    build_inline_layout_into(
                        &self.nodes,
                        layout_ctx_mut,
                        font_ctx_mut,
                        &mut layout,
                        self.viewport.scale(),
                        task.node_id,
                    );

                    #[cfg(feature = "parallel-construct")]
                    {
                        LAYOUT_CTX.set(Some(layout_ctx));
                    }

                    // If layout doesn't contain any inline boxes, then it is safe to populate the content_widths
                    // cache during this parallelized stage.
                    // if layout.layout.inline_boxes().is_empty() {
                    //     layout.content_widths();
                    // }

                    ConstructionTaskResult {
                        node_id: task.node_id,
                        data: ConstructionTaskResultData::InlineLayout(layout),
                    }
                }
            })
            .collect();

        for result in results {
            match result.data {
                ConstructionTaskResultData::InlineLayout(layout) => {
                    self.nodes[result.node_id].cache_mut().clear();
                    self.nodes[result.node_id]
                        .element_data_mut()
                        .unwrap()
                        .inline_layout_data = Some(layout);
                }
            }
        }

        self.deferred_construction_nodes.clear();
    }

    /// Walk the nodes now that they're properly styled and transfer their styles to the taffy style system
    ///
    /// TODO: update taffy to use an associated type instead of slab key
    /// TODO: update taffy to support traited styles so we don't even need to rely on taffy for storage
    pub fn resolve_layout(&mut self) {
        let size = self.stylist.device().au_viewport_size();

        let available_space = taffy::Size {
            width: AvailableSpace::Definite(size.width.to_f32_px()),
            height: AvailableSpace::Definite(size.height.to_f32_px()),
        };

        let root_element_id = crate::taffy_node_id(self.root_element().id);

        // println!("\n\nRESOLVE LAYOUT\n===========\n");

        taffy::compute_root_layout(self, root_element_id, available_space);
        taffy::round_layout(self, root_element_id);

        // Taffy currently maps CSS `position: fixed` to absolute positioning,
        // which leaves the box relative to its DOM layout parent. A portal
        // mounted after a full-height application root therefore starts one
        // viewport below the window even with `top: 0`. Cancel the layout
        // parent's document-space offset so fixed boxes use the viewport as
        // their containing block, as CSS requires.
        let fixed_nodes = self
            .nodes
            .iter()
            .filter_map(|(node_id, node)| {
                let is_fixed = node
                    .primary_styles()
                    .is_some_and(|style| style.clone_position() == Position::Fixed);
                is_fixed.then_some((node_id, node.layout_parent.get()))
            })
            .collect::<Vec<_>>();

        for (node_id, parent_id) in fixed_nodes {
            let Some(parent_id) = parent_id else {
                continue;
            };
            let parent_position = self.nodes[parent_id].absolute_position(0.0, 0.0);
            self.nodes[node_id].final_layout_mut().location.x -= parent_position.x;
            self.nodes[node_id].final_layout_mut().location.y -= parent_position.y;
        }

        // println!("\n\n");
        // taffy::print_tree(self, root_node_id)
    }
}

//! Resolve style and layout

use blitz_traits::node_id::NodeId;
use std::{
    cell::RefCell,
    collections::HashSet,
    time::{SystemTime, UNIX_EPOCH},
};

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
    /// Pull every scroll offset back inside the content it scrolls.
    ///
    /// Scrolling clamps against the extent at the time of the gesture, and
    /// nothing re-checked it afterwards. So any layout that made a scroller's
    /// content *shorter* left the offset beyond the new end, and the view
    /// stayed parked in space the content no longer reaches: dismiss a panel
    /// while scrolled to the bottom and its height is simply gone from under
    /// you, leaving a band of nothing between the last content and the edge.
    /// Far enough past the end and there is nothing left to see at all.
    ///
    /// Done after layout, which is the only point at which the new extents are
    /// known, and cheap: offsets are almost always zero.
    fn clamp_scroll_offsets(&mut self) {
        for (_, node) in self.nodes.iter_mut() {
            // The accessor panics on node kinds that have no scroll offset, so
            // ask the data first rather than every node in the tree.
            let Some(offset) = node
                .data
                .downcast_element()
                .map(|element| element.scroll_offset)
            else {
                continue;
            };
            if offset.x == 0.0 && offset.y == 0.0 {
                continue;
            }
            let max_x = f64::from(node.final_layout().scroll_width()).max(0.0);
            let max_y = f64::from(node.final_layout().scroll_height()).max(0.0);
            let clamped = node.scroll_offset_mut();
            clamped.x = offset.x.clamp(0.0, max_x);
            clamped.y = offset.y.clamp(0.0, max_y);
        }
    }

    /// Re-break any inline layout whose lines belong to a pass other than the
    /// one that decided its box.
    ///
    /// Taffy performs layout under min-content and max-content constraints
    /// while sizing a box, and every one of those passes breaks the same parley
    /// layout the screen reads from. Whichever ran last is what gets painted.
    /// That is usually the real layout, and when the final pass is answered
    /// from the taffy cache it is not: `compute_inline_layout` never runs
    /// again, and the trial break stays. Reported as "1st load is fucked" and
    /// measured on a live transcript as paragraphs broken at 164px inside a
    /// 1,426px box, 39 lines of one or two words each.
    ///
    /// Cheap by construction: it compares two floats per inline root and
    /// re-breaks only the ones that disagree, which in a settled document is
    /// none of them.
    /// Returns whether any repair changed a layout's height, which means the
    /// boxes taffy sized are now wrong and layout has to run again.
    fn repair_inline_line_breaks(&mut self) -> bool {
        let scale = self.viewport.scale();

        let mut wrong = Vec::new();
        for (node_id, node) in self.nodes.iter() {
            let Some(inline) = node
                .data
                .downcast_element()
                .and_then(|element| element.inline_layout_data.as_ref())
            else {
                continue;
            };
            // The *unrounded* layout, which is the width the layout pass
            // broke at. `final_layout` is rounded to whole pixels, and half a
            // pixel of rounding-down is enough to wrap a label that exactly
            // fit: "125.1k / 200.0k ctx · 63%" came back on two lines.
            let layout = node.unrounded_layout();
            let content_width = (layout.size.width
                - layout.padding.left
                - layout.padding.right
                - layout.border.left
                - layout.border.right)
                .max(0.0)
                * scale;
            // Half a device pixel: below that the break is identical and
            // re-running it would cost a frame to change nothing.
            if inline
                .laid_out_at
                .is_none_or(|broken_at| (broken_at - content_width).abs() > 0.5)
            {
                wrong.push((node_id, content_width));
            }
        }

        let mut changed_height = false;
        for (node_id, content_width) in wrong {
            // Breaking discards the alignment the layout pass applied, so it
            // has to go back on: without it every centred or right-aligned
            // paragraph this touches would silently come back left-aligned.
            let alignment = self.nodes[node_id]
                .primary_styles()
                .map(|style| {
                    use parley::layout::Alignment;
                    use style::values::specified::TextAlignKeyword;
                    match style.clone_text_align() {
                        TextAlignKeyword::Start => Alignment::Start,
                        TextAlignKeyword::Left | TextAlignKeyword::MozLeft => Alignment::Left,
                        TextAlignKeyword::Right | TextAlignKeyword::MozRight => Alignment::Right,
                        TextAlignKeyword::Center | TextAlignKeyword::MozCenter => Alignment::Center,
                        TextAlignKeyword::Justify => Alignment::Justify,
                        TextAlignKeyword::End => Alignment::End,
                    }
                })
                .unwrap_or(parley::layout::Alignment::Start);

            let Some(inline) = self.nodes[node_id]
                .data
                .downcast_element_mut()
                .and_then(|element| element.inline_layout_data.as_mut())
            else {
                continue;
            };
            inline.layout.break_all_lines(Some(content_width));
            inline.layout.align(
                alignment,
                parley::layout::AlignmentOptions {
                    align_when_overflowing: false,
                },
            );
            inline.laid_out_at = Some(content_width);

            // Any repair at all invalidates the boxes around it, not just one
            // whose parley height moved. The box was sized by a pass that broke
            // these lines differently, and its height was accumulated into
            // every ancestor's content size on the way up. Comparing parley
            // heights before and after missed that: the layout being repaired
            // is not the one the box was sized from, so it can come out the
            // same height while the box is still wrong. Measured live as a
            // transcript whose content ran 1,062px past the extent it reported,
            // so it could not scroll to its own last message.
            changed_height = true;
            self.nodes[node_id].insert_damage(RestyleDamage::RELAYOUT);
        }

        changed_height
    }

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
        #[cfg(feature = "log-phase-times")]
        let mut timer =
            debug_timer::RealDebugTimer::init_if(blitz_traits::profiling::deep_profiling_enabled());
        #[cfg(not(feature = "log-phase-times"))]
        let mut timer = debug_timer::DummyDebugTimer::init();
        #[cfg(feature = "log-phase-times")]
        crate::layout::layout_counters::begin(blitz_traits::profiling::deep_profiling_enabled());

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
        //
        // Caught, under `BLITZ_TRACE_LAYOUT_PANIC=1` only, so the markup that
        // killed layout can be printed before the process goes. The panic hook
        // that names the element runs without the document and can only give an
        // id and a class list; the id is worthless once the process is gone,
        // and a class list is not markup you can put in a test. This is the one
        // place that still holds `&mut self` when layout fails, so it is the
        // only place the subtree can be serialized. The panic is resumed
        // immediately: nothing here makes a failed layout survivable.
        #[cfg(not(target_arch = "wasm32"))]
        if crate::layout::layout_panic_probe::enabled() {
            let attempt =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.resolve_layout()));
            if let Err(payload) = attempt {
                if let Some(node_id) = crate::layout::layout_panic_probe::innermost_node() {
                    if let Some(node) = self.nodes.get(node_id) {
                        eprintln!(
                            "[blitz-layout-panic] markup of node {node_id}:\n{}",
                            node.outer_html_pretty()
                        );
                    }
                }
                std::panic::resume_unwind(payload);
            }
        } else {
            self.resolve_layout();
        }
        #[cfg(target_arch = "wasm32")]
        self.resolve_layout();
        self.correct_hoisted_fixed_positions();
        self.resolve_hoisted_clips();
        timer.record_time("layout");

        // One extra pass, only when a repair moved a box. Bounded deliberately:
        // the second layout runs against lines that already agree with their
        // widths, so a third could not find anything new, and an unbounded loop
        // here would be a hang rather than a slow frame.
        if self.repair_inline_line_breaks() {
            // Damage first. The repair marks the nodes it touched, but a box's
            // height is accumulated into every ancestor's content size on the
            // way up, and those ancestors answer from the taffy cache until
            // damage propagation clears it. Without this the second pass runs
            // and changes nothing: measured live as a scroller still reporting
            // an extent 1,062px short of its own content.
            if self.incremental_layout {
                self.propagate_damage_flags(root_node_id, RestyleDamage::empty());
            }
            self.flush_styles_to_layout(root_node_id);
            self.resolve_layout();
            self.correct_hoisted_fixed_positions();
            self.resolve_hoisted_clips();
            self.repair_inline_line_breaks();
            self.resolve_transforms(root_node_id);
        }

        self.clamp_scroll_offsets();
        self.trace_escaped_inline_fragments();

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

        let mut subdoc_animation_pacing = crate::document::AnimationPacing::Idle;
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

                subdoc_animation_pacing = subdoc_animation_pacing.max(sub_doc.animation_pacing());
            }
        }
        self.subdoc_animation_pacing = subdoc_animation_pacing;
        timer.record_time("subdocs");

        // Printed with the phases so a single line says both how long layout
        // took and how much of the tree it touched. Without the counts the
        // timings cannot distinguish a few slow nodes from a cache miss across
        // the document, and those need opposite fixes.
        #[cfg(feature = "log-phase-times")]
        {
            // The offenders are read, and the message built, only when a sink
            // is configured: the counters are cheap to keep and expensive to
            // describe, and this feature now travels with a shipped binary.
            // Draining, though, is unconditional — `layout_counters::last()` is
            // what the benchmarks read, and counts that are never taken keep
            // accumulating across resolves.
            let describe = timer.is_logging();
            if describe {
                // Named before the counters are drained, and only when the pass
                // was expensive enough to be worth looking at.
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
                                .map(|node| format!("{:?}", node.style().display))
                                .unwrap_or_default();
                            format!("{id:?}:{tag}({display})x{count}")
                        })
                        .collect();
                    debug_timer::log_line(&format!("  layout hotspots: {}\n", described.join(" ")));
                }
            }
            let counts = crate::layout::layout_counters::take();
            if describe {
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

        // Text overflows too, and only layout *children* were counted above.
        //
        // Glyph runs are not nodes, so a `white-space: nowrap` line wider than
        // its box left `scrollable_overflow` exactly equal to that box. Paint
        // skips its clip layer when the overflow rect fits the border box —
        // most `overflow-hidden` wrappers really do clip nothing, and a layer
        // is the most expensive thing in a frame — so the one case that needed
        // the clip was the one case that reported it was unnecessary. A
        // truncated tab title painted straight through the close button beside
        // it, a branch name through the chip after it, and a transcript line
        // under the cost readout: measured here as a 150px box painting its
        // text out to x=354.
        if let Some(inline_layout) = self.nodes[node_id]
            .data
            .downcast_element()
            .and_then(|element| element.inline_layout_data.as_ref())
        {
            // Already device pixels: parley is handed the scaled size, so
            // scaling again doubled every inline root's overflow at 2x and
            // inflated its hit area with it.
            let text_width = inline_layout.layout.width() as f64;
            let text_height = inline_layout.layout.height() as f64;
            overflow = overflow.union(Rect::new(0.0, 0.0, text_width, text_height));
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

            // A hidden subtree keeps the boxes it already has.
            //
            // `display: none` means "do not lay this out", not "forget what you
            // know about it". Collecting layout children for a hidden container
            // yields an empty list, so hiding a pane used to discard every box
            // and every shaped inline layout beneath it, and revealing it built
            // all of them again from the DOM. In an application that retains
            // its tabs and toggles them by class, that is the entire cost of a
            // tab switch, paid again on every switch: measured on six retained
            // panes of a real project tab, a *re-reveal* cost exactly what the
            // first reveal cost, 46,526 layout computations either way.
            //
            // Damage is deliberately left in place rather than cleared. Content
            // that changes while hidden still carries its damage to the reveal,
            // where the normal path reconstructs precisely what changed.
            if doc.incremental_layout
                && doc.nodes[node_id].is_display_none()
                && doc.nodes[node_id].layout_children.borrow().is_some()
            {
                return;
            }

            // A node that has never been constructed has no boxes to keep, and
            // no damage either once its styles survive being hidden: a pane
            // that was hidden before it was ever shown reaches its reveal with
            // valid styles, nothing marked dirty, and nothing to lay out. It
            // used to be rescued by stylo discarding those styles. Ask the
            // boxes instead of the damage.
            let never_constructed = doc.nodes[node_id].layout_children.borrow().is_none();

            if !doc.incremental_layout
                || never_constructed
                || damage.intersects(CONSTRUCT_FC | CONSTRUCT_BOX)
            {
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

        // Drop nodes that are no longer fixed, and keep the rest.
        //
        // This used to `clear()` and rebuild, which worked exactly once. The
        // loop below reads `layout_parent` to learn where a node came from, but
        // the hoist itself sets that to the root, so on the second pass every
        // already-hoisted node takes the `parent_id == root_id` branch and is
        // never re-recorded. Combined with the clear, the map came back empty
        // and `flush_styles_to_layout` put the layer in the root's stacking
        // context instead of the one its box tree gives it.
        //
        // The symptom was a full-bleed background that painted correctly on the
        // first frame and disappeared on the next relayout, which on a real
        // page means as soon as an image finishes loading.
        let still_fixed: HashSet<NodeId> = hoisted.iter().copied().collect();
        self.hoisted_fixed_parents
            .retain(|node_id, _| still_fixed.contains(node_id));

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

                // A hidden subtree generates no boxes, so nothing in it may be
                // hoisted. This walk had no display check because it could not
                // reach a hidden subtree: hiding a pane emptied its layout
                // children and stylo discarded its styles, so the recursion
                // stopped and `primary_styles` returned None. Now that a hidden
                // pane keeps both, every `position: fixed` element in every
                // background tab was hoisted onto the root and painted over the
                // tab in front, one ghost per retained tab.
                if styles.clone_display().is_none() {
                    continue;
                }

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

    /// Turn each hoisted child's clipping ancestors into rectangles paint can
    /// use, relative to the origin of the stacking context it paints in.
    ///
    /// Separate from `flush_styles_to_layout`, which decides *which* ancestors
    /// clip: that runs before taffy, when every box is still zero-sized, so
    /// reading a size there produced an empty clip and made hoisted content
    /// disappear entirely rather than merely escape.
    pub(crate) fn resolve_hoisted_clips(&mut self) {
        if self.hoisted_clip_hosts.is_empty() {
            return;
        }

        // By index, leaving the list in place: it belongs to the last flush,
        // and layout can run more than once against it.
        for index in 0..self.hoisted_clip_hosts.len() {
            let host = self.hoisted_clip_hosts[index];
            let Some(mut context) = self.nodes[host].stacking_context.take() else {
                continue;
            };
            let host_position = self.nodes[host].absolute_position(0.0, 0.0);

            for child in context.children.iter_mut() {
                child.clips.clear();
                child.clips.reserve(child.clip_ancestors.len());
                for &clipper in child.clip_ancestors.iter() {
                    let node = &self.nodes[clipper];
                    // The clip is the clipping box's own border box, so its
                    // own scroll offset does not enter into it. Ancestor
                    // scrolling does, and `absolute_position` applies that.
                    let position = node.absolute_position(0.0, 0.0);
                    let layout = node.final_layout();
                    let left = position.x - host_position.x;
                    let top = position.y - host_position.y;
                    // The padding box, matching what paint clips content to.
                    child.clips.push(taffy::Rect {
                        left: left + layout.border.left,
                        top: top + layout.border.top,
                        right: left + layout.size.width - layout.border.right,
                        bottom: top + layout.size.height - layout.border.bottom,
                    });
                }
            }

            self.nodes[host].stacking_context = Some(context);
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
                    // The node and every layout ancestor. The shaped layout
                    // that lands here has not been broken into lines yet, and
                    // an ancestor still holding a cached layout never descends,
                    // so clearing this node alone leaves the fresh unbroken
                    // layout in place with nothing to break it. Non-atomic
                    // inline elements then report geometry from a single line
                    // as wide as the whole paragraph.
                    //
                    // `layout_parent`, not `parent`: taffy walks the layout
                    // tree, and anonymous blocks make the two chains differ.
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

use blitz_traits::node_id::NodeId;
use std::ops::Range;

use crate::Node;
use crate::net::ResourceHandler;
use crate::node::NodeFlags;
use crate::{
    BaseDocument, net::ImageHandler, node::ImageResourceData, node::Status, util::ImageLayerKind,
};
use style::properties::ComputedValues;
use style::properties::generated::longhands::position::computed_value::T as Position;
use style::selector_parser::RestyleDamage;
use style::servo_arc::Arc as ServoArc;
use style::url::ComputedUrl;
use style::values::computed::Float;
use style::values::computed::Overflow as StyloOverflow;
use style::values::generics::image::Image as StyloImage;
use style::values::specified::align::AlignFlags;
use style::values::specified::box_::DisplayInside;
use style::values::specified::box_::DisplayOutside;
use taffy::Rect;
use thin_vec::ThinVec;

pub(crate) const CONSTRUCT_BOX: RestyleDamage =
    RestyleDamage::from_bits_retain(0b_0000_0000_0001_0000);
pub(crate) const CONSTRUCT_FC: RestyleDamage =
    RestyleDamage::from_bits_retain(0b_0000_0000_0010_0000);
pub(crate) const CONSTRUCT_DESCENDENT: RestyleDamage =
    RestyleDamage::from_bits_retain(0b_0000_0000_0100_0000);

pub(crate) const ONLY_RELAYOUT: RestyleDamage =
    RestyleDamage::from_bits_retain(0b_0000_0000_0000_1000);

pub(crate) const ALL_DAMAGE: RestyleDamage =
    RestyleDamage::from_bits_retain(0b_0000_0000_0111_1111);

/// `BLITZ_SUBTREE_SKIP=0` restores the unconditional walk.
///
/// The skip below is the sharpest correctness edge in this file: a subtree
/// that is quietly not flushed lays out from a stale taffy style, and the
/// symptom is a pane of wrong geometry rather than a crash. One binary that
/// can be run both ways settles "is it the skip?" in two launches instead of
/// a rebuild, which is what it was worth the day tab switching felt wrong —
/// the answer then was no, and having the answer cheaply was the point.
fn subtree_skip_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("BLITZ_SUBTREE_SKIP").as_deref() != Ok("0"))
}

impl BaseDocument {
    pub(crate) fn propagate_damage_flags(
        &mut self,
        node_id: NodeId,
        damage_from_parent: RestyleDamage,
    ) -> RestyleDamage {
        let mut damage = if let Some(data) = self.nodes[node_id]
            .stylo_element_data_opt_mut()
            .and_then(|s| s.get_mut())
        {
            data.damage
        } else {
            return RestyleDamage::empty();
        };
        // Read before anything is folded in, which is the only moment "this
        // node changed" is separable from "something under it changed". Paint
        // damage needs the former: after the loop below, every ancestor up to
        // the root carries its descendants' damage.
        if !damage.is_empty() {
            self.paint_damage.note_own_damage(node_id);
        }
        damage |= damage_from_parent;

        // Flush updated pseudo-element styles to their anonymous nodes so that
        // style changes which don't trigger box construction still take effect.
        //
        // TODO: see if this can be made more efficient (/run less often)
        self.sync_pseudo_element_styles(node_id);

        let damage_for_children = RestyleDamage::empty();
        let children = std::mem::take(&mut self.nodes[node_id].children);
        let layout_children = std::mem::take(self.nodes[node_id].layout_children.get_mut());
        let use_layout_children = self.nodes[node_id].should_traverse_layout_children();
        if use_layout_children {
            let layout_children = layout_children.as_ref().unwrap();
            for child in layout_children.iter() {
                damage |= self.propagate_damage_flags(*child, damage_for_children);
            }
        } else {
            for child in children.iter() {
                damage |= self.propagate_damage_flags(*child, damage_for_children);
            }
            if let Some(before_id) = self.nodes[node_id].before() {
                damage |= self.propagate_damage_flags(before_id, damage_for_children);
            }
            if let Some(after_id) = self.nodes[node_id].after() {
                damage |= self.propagate_damage_flags(after_id, damage_for_children);
            }
        }

        let node = &mut self.nodes[node_id];

        // Put children back
        node.children = children;
        *node.layout_children.get_mut() = layout_children;

        if damage.contains(CONSTRUCT_BOX) {
            damage.insert(RestyleDamage::RELAYOUT);
        }

        // Compute damage to propagate to parent
        let damage_for_parent = damage; // & RestyleDamage::RELAYOUT;

        // If the node or any of it's children have been mutated or their layout styles
        // have changed, then we should clear it's layout cache.
        if damage.intersects(ONLY_RELAYOUT | CONSTRUCT_BOX) {
            #[cfg(feature = "log-phase-times")]
            crate::layout::layout_counters::note_cache_cleared();
            node.cache_mut().clear();
            if let Some(inline_layout) = node
                .data
                .downcast_element_mut()
                .and_then(|el| el.inline_layout_data.as_mut())
            {
                inline_layout.content_widths = None;
            }
            damage.remove(ONLY_RELAYOUT);
        }

        // Store damage for current node
        node.set_damage(damage);

        // let _is_fc_root = node
        //     .primary_styles()
        //     .map(|s| is_fc_root(&s))
        //     .unwrap_or(false);

        // if damage.contains(CONSTRUCT_BOX) {
        //     // damage_for_parent.insert(CONSTRUCT_FC | CONSTRUCT_DESCENDENT);
        //     damage_for_parent.insert(CONSTRUCT_BOX);
        // }

        // if damage.contains(CONSTRUCT_FC) {
        //     damage_for_parent.insert(CONSTRUCT_DESCENDENT);
        //     // if !is_fc_root {
        //     damage_for_parent.insert(CONSTRUCT_FC);
        //     // }
        // }

        // Propagate damage to parent
        damage_for_parent
    }

    /// Flush updated pseudo-element (`::before`/`::after`) styles from the owning
    /// element's stylo data to the pseudo-element's anonymous node.
    ///
    /// Pseudo-element styles are normally flushed to the pseudo-element's node
    /// during box construction (see `flush_pseudo_elements`), but in incremental
    /// mode box construction only runs for nodes with construction damage.
    /// Pseudo-element style changes which don't require reconstruction (e.g.
    /// animations/transitions of repaint- or relayout-only properties) must still
    /// be flushed to the pseudo-element's node - along with the damage they imply -
    /// so that layout and paint see the new style.
    fn sync_pseudo_element_styles(&mut self, node_id: NodeId) {
        let node = &self.nodes[node_id];

        let before_node_id = node.before();
        let after_node_id = node.after();
        if before_node_id.is_none() && after_node_id.is_none() {
            return;
        }

        let (before_style, after_style) = {
            let style_data = node.stylo_element_data_opt().and_then(|s| s.get());
            let Some(style_data) = style_data.as_ref() else {
                return;
            };
            // Note: yes these are kinda backwards (see `flush_pseudo_elements`)
            let pseudos = style_data.styles.pseudos.as_array();
            (pseudos[1].clone(), pseudos[0].clone())
        };

        // Creation and removal of pseudo-elements is handled during box construction
        // (Stylo generates construction damage for those cases), so only the case
        // where the pseudo-element both was and remains present is handled here.
        for (pe_node_id, pe_style) in [(before_node_id, before_style), (after_node_id, after_style)]
        {
            let (Some(pe_node_id), Some(pe_style)) = (pe_node_id, pe_style) else {
                continue;
            };
            let mut pe_data = self.nodes[pe_node_id]
                .stylo_element_data_opt_mut()
                .and_then(|s| s.get_mut());
            let Some(pe_data) = pe_data.as_mut() else {
                continue;
            };
            let Some(old_style) = pe_data.styles.primary.clone() else {
                continue;
            };
            if std::ptr::eq(&*old_style, &*pe_style) {
                continue;
            }

            let diff = RestyleDamage::compute_style_difference::<&Node>(&old_style, &pe_style);
            pe_data.damage.insert(diff.damage);
            pe_data.styles.primary = Some(pe_style);
            pe_data.set_restyled();
        }
    }
}

// fn is_fc_root(style: &ComputedValues) -> bool {
//     let display = style.clone_display();
//     let display_inside = display.inside();

//     match display_inside {
//         DisplayInside::Flow => {
//             // Depends on parent context
//             false
//         }

//         DisplayInside::None => true,
//         DisplayInside::FlowRoot => true,
//         DisplayInside::Flex => true,
//         DisplayInside::Grid => true,
//         DisplayInside::Table => true,
//         DisplayInside::TableCell => true,

//         DisplayInside::Contents => false,
//         DisplayInside::TableRowGroup => false,
//         DisplayInside::TableColumn => false,
//         DisplayInside::TableColumnGroup => false,
//         DisplayInside::TableHeaderGroup => false,
//         DisplayInside::TableFooterGroup => false,
//         DisplayInside::TableRow => false,
//     }
// }

pub(crate) fn compute_layout_damage(old: &ComputedValues, new: &ComputedValues) -> RestyleDamage {
    let box_tree_needs_rebuild = || {
        let old_box = old.get_box();
        let new_box = new.get_box();

        if old_box.display != new_box.display
            || old_box.float != new_box.float
            || old_box.position != new_box.position
            || old.clone_visibility() != new.clone_visibility()
        {
            return true;
        }

        if old.get_font() != new.get_font() {
            return true;
        }

        if new_box.display.outside() == DisplayOutside::Block
            && new_box.display.inside() == DisplayInside::Flow
        {
            let alignment_establishes_new_block_formatting_context = |style: &ComputedValues| {
                style.get_position().align_content.primary() != AlignFlags::NORMAL
            };

            let old_column = old.get_column();
            let new_column = new.get_column();
            if old_box.overflow_x.is_scrollable() != new_box.overflow_x.is_scrollable()
                || old_column.is_multicol() != new_column.is_multicol()
                || old_column.column_span != new_column.column_span
                || alignment_establishes_new_block_formatting_context(old)
                    != alignment_establishes_new_block_formatting_context(new)
            {
                return true;
            }
        }

        if old_box.display.is_list_item() {
            let old_list = old.get_list();
            let new_list = new.get_list();
            if old_list.list_style_position != new_list.list_style_position
                || old_list.list_style_image != new_list.list_style_image
                || (new_list.list_style_image == StyloImage::None
                    && old_list.list_style_type != new_list.list_style_type)
            {
                return true;
            }
        }

        if new.is_pseudo_style() && old.get_counters().content != new.get_counters().content {
            return true;
        }

        false
    };

    let text_shaping_needs_recollect = || {
        if old.clone_direction() != new.clone_direction()
            || old.clone_unicode_bidi() != new.clone_unicode_bidi()
        {
            return true;
        }

        let old_text = old.get_inherited_text();
        let new_text = new.get_inherited_text();
        if !std::ptr::eq(old_text, new_text)
            && (old_text.white_space_collapse != new_text.white_space_collapse
                || old_text.text_transform != new_text.text_transform
                || old_text.word_break != new_text.word_break
                || old_text.overflow_wrap != new_text.overflow_wrap
                || old_text.letter_spacing != new_text.letter_spacing
                || old_text.word_spacing != new_text.word_spacing
                || old_text.text_rendering != new_text.text_rendering)
        {
            return true;
        }

        false
    };

    #[allow(
        clippy::if_same_then_else,
        reason = "these branches will soon be different"
    )]
    if box_tree_needs_rebuild() {
        ALL_DAMAGE
    } else if text_shaping_needs_recollect() {
        ALL_DAMAGE
    } else {
        // This element needs to be laid out again, but does not have any damage to
        // its box. In the future, we will distinguish between types of damage to the
        // fragment as well.
        RestyleDamage::RELAYOUT
    }
}

/// A child with a z_index that is hoisted up to it's containing Stacking Context for paint purposes
#[derive(Debug, Clone)]
pub struct HoistedPaintChild {
    pub node_id: NodeId,
    pub z_index: i32,
    pub position: taffy::Point<f32>,
    /// The ancestors this child was hoisted past whose overflow clips it.
    ///
    /// Hoisting moves a node out of the subtree whose clip layers would have
    /// contained it, so without these it paints over anything its ancestors
    /// were meant to cut it off at. Empty for the overwhelming majority of
    /// hoisted children, which cross nothing that clips.
    ///
    /// Ids, not rectangles: this is collected before taffy runs, when every
    /// box is still zero-sized. `resolve_hoisted_clips` turns them into
    /// [`Self::clips`] once there is a layout to read.
    pub clip_ancestors: Vec<NodeId>,
    /// Where [`Self::clip_ancestors`] ended up, relative to the origin of the
    /// stacking context this child paints in.
    pub clips: Vec<taffy::Rect<f32>>,
    /// Whether an ancestor's overflow clips this child from here upward.
    ///
    /// An ancestor does not clip a positioned box whose containing block is
    /// outside that ancestor (CSS 2.1 11.1.1), so for an absolutely positioned
    /// child this starts false and turns on at its containing block: an
    /// `overflow: hidden` wrapper *between* the child and the box it is
    /// positioned against has no say over it. In-flow children are clipped by
    /// everything above them, and `position: fixed` by nothing, its containing
    /// block being the viewport.
    pub clips_apply: bool,
    /// Absolutely positioned, so `clips_apply` turns on at its containing block.
    pub starts_at_containing_block: bool,
}

impl HoistedPaintChild {
    fn new(node_id: NodeId, z_index: i32, position: Position) -> Self {
        Self {
            node_id,
            z_index,
            position: taffy::Point::ZERO,
            clip_ancestors: Vec::new(),
            clips: Vec::new(),
            clips_apply: !matches!(position, Position::Absolute | Position::Fixed),
            starts_at_containing_block: position == Position::Absolute,
        }
    }
}

#[derive(Debug)]
pub struct HoistedPaintChildren {
    pub children: Vec<HoistedPaintChild>,
    /// The number of hoisted point children with negative z_index
    pub negative_z_count: u32,

    pub content_area: taffy::Rect<f32>,
}

impl HoistedPaintChildren {
    fn new() -> Self {
        Self {
            children: Vec::new(),
            negative_z_count: 0,
            content_area: taffy::Rect::ZERO,
        }
    }

    pub fn reset(&mut self) {
        self.children.clear();
        self.negative_z_count = 0;
    }

    pub fn compute_content_size(&mut self, doc: &BaseDocument) {
        fn child_pos(child: &HoistedPaintChild, doc: &BaseDocument) -> Rect<f32> {
            let node = &doc.nodes[child.node_id];
            let left = child.position.x + node.final_layout().location.x;
            let top = child.position.y + node.final_layout().location.y;
            let right = left + node.final_layout().size.width;
            let bottom = top + node.final_layout().size.height;

            taffy::Rect {
                top,
                left,
                bottom,
                right,
            }
        }

        if self.children.is_empty() {
            self.content_area = taffy::Rect::ZERO;
        } else {
            self.content_area = child_pos(&self.children[0], doc);
            for child in self.children[1..].iter() {
                let pos = child_pos(child, doc);
                self.content_area.left = self.content_area.left.min(pos.left);
                self.content_area.top = self.content_area.top.min(pos.top);
                self.content_area.right = self.content_area.right.max(pos.right);
                self.content_area.bottom = self.content_area.bottom.max(pos.bottom);
            }
        }
    }

    pub fn sort(&mut self) {
        self.children.sort_by_key(|c| c.z_index);
        self.negative_z_count = self.children.iter().take_while(|c| c.z_index < 0).count() as u32;
    }

    pub fn neg_z_range(&self) -> Range<usize> {
        0..(self.negative_z_count as usize)
    }

    pub fn pos_z_range(&self) -> Range<usize> {
        (self.negative_z_count as usize)..self.children.len()
    }

    pub fn neg_z_hoisted_children(
        &self,
    ) -> impl ExactSizeIterator<Item = &HoistedPaintChild> + DoubleEndedIterator {
        self.children[self.neg_z_range()].iter()
    }

    pub fn pos_z_hoisted_children(
        &self,
    ) -> impl ExactSizeIterator<Item = &HoistedPaintChild> + DoubleEndedIterator {
        self.children[self.pos_z_range()].iter()
    }
}

impl BaseDocument {
    pub(crate) fn invalidate_inline_contexts(&mut self) {
        let scale = self.viewport.scale();

        let font_ctx = &self.font_ctx;
        let layout_ctx = &mut self.layout_ctx;

        let mut anon_nodes = Vec::new();

        for (_, node) in self.nodes.iter_mut() {
            if !(node.flags.contains(NodeFlags::IS_IN_DOCUMENT)) {
                continue;
            }

            let Some(element) = node.data.downcast_element_mut() else {
                continue;
            };

            if element.inline_layout_data.is_some() {
                if node.is_anonymous() {
                    anon_nodes.push(node.id);
                } else {
                    node.insert_damage(ALL_DAMAGE);
                }
            } else if let Some(input) = element.text_input_data_mut() {
                input.editor.set_scale(scale);
                let mut font_ctx = font_ctx.lock().unwrap();
                input.editor.refresh_layout(&mut font_ctx, layout_ctx);
                // The placeholder is a second editor and needs the same scale.
                // Left behind, it keeps whatever scale it was cloned at and its
                // glyphs are painted at that size while everything around them
                // is painted at the new one: on a retina display the
                // placeholder comes out half size.
                if let Some(placeholder) = input.placeholder_editor.as_mut() {
                    placeholder.set_scale(scale);
                    placeholder.refresh_layout(&mut font_ctx, layout_ctx);
                }
                node.insert_damage(ONLY_RELAYOUT);
            }
        }

        for node_id in anon_nodes {
            if let Some(parent_id) = *(self.nodes[node_id].layout_parent.get_mut()) {
                self.nodes[parent_id].insert_damage(ALL_DAMAGE);
            }
        }
    }

    pub fn flush_styles_to_layout(&mut self, node_id: NodeId) {
        // Rebuilt by the walk below, and stale otherwise: an incremental flush
        // can rebuild a context whose hoisted children no longer cross
        // anything that clips.
        self.hoisted_clip_hosts.clear();
        self.flush_styles_to_layout_impl(node_id, None);
    }

    /// Flush a CSS image layer list (`background-image` or `mask-image`) from style
    /// to dedicated storage on the node, fetching any images which are not yet loaded.
    fn flush_image_layers_from_style(&mut self, node_id: NodeId, kind: ImageLayerKind) {
        let doc_id = self.id();
        let node = self.nodes.get_mut(node_id).unwrap();
        // Clone the primary style `Arc` into an owned value so the immutable
        // borrow of `node` (held by the stylo element data guard) is released
        // before we take a mutable borrow of `node.data` below.
        let style = {
            let stylo_element_data = node.stylo_element_data_opt().and_then(|s| s.get());
            let primary_styles = stylo_element_data
                .as_ref()
                .and_then(|data| data.styles.get_primary());
            let Some(style) = primary_styles else {
                return;
            };
            style.clone()
        };
        let Some(elem) = node.data.downcast_element_mut() else {
            return;
        };

        let (style_images, elem_images) = match kind {
            ImageLayerKind::Background => (
                &style.get_background().background_image.0,
                &mut elem.background_images,
            ),
            ImageLayerKind::Mask => (&style.get_svg().mask_image.0, &mut elem.mask_images),
        };

        let len = style_images.len();
        elem_images.resize_with(len, || None);

        for idx in 0..len {
            let style_image = &style_images[idx];
            let new_image = match style_image {
                StyloImage::Url(ComputedUrl::Valid(new_url)) => {
                    let old_image = elem_images[idx].as_ref();
                    let old_image_url = old_image.map(|data| &data.url);
                    if old_image_url.is_some_and(|old_url| **new_url == **old_url) {
                        break;
                    }

                    // Check cache first
                    let url_str = new_url.as_str();
                    if let Some(cached_image) = self.image_cache.get(url_str) {
                        #[cfg(feature = "tracing")]
                        tracing::info!("Loading image {url_str} from cache");
                        Some(ImageResourceData {
                            url: new_url.clone(),
                            status: Status::Ok,
                            image: cached_image.clone(),
                        })
                    } else if let Some(waiting_list) = self.pending_images.get_mut(url_str) {
                        // Image is already being fetched, queue this node
                        #[cfg(feature = "tracing")]
                        tracing::info!("Image {url_str} already pending, queueing node {node_id}");
                        waiting_list.push((node_id, kind.image_type(idx)));
                        Some(ImageResourceData::new(new_url.clone()))
                    } else {
                        // Start fetch and track as pending
                        #[cfg(feature = "tracing")]
                        tracing::info!("Fetching image {url_str}");
                        self.pending_images
                            .insert(url_str.to_string(), vec![(node_id, kind.image_type(idx))]);

                        self.net_provider.fetch(
                            doc_id,
                            crate::net::stamped_request(
                                (**new_url).clone(),
                                self.abort_signal.as_ref(),
                            ),
                            ResourceHandler::boxed(
                                self.tx.clone(),
                                doc_id,
                                None, // Don't pass node_id, we'll handle via pending_images
                                self.shell_provider.clone(),
                                ImageHandler::new(kind.image_type(idx)),
                            ),
                        );

                        Some(ImageResourceData::new(new_url.clone()))
                    }
                }
                _ => None,
            };

            // Element will always exist due to resize_with above
            elem_images[idx] = new_image;
        }
    }

    /// Walk the whole tree, converting styles to layout
    fn flush_styles_to_layout_impl(
        &mut self,
        node_id: NodeId,
        parent_stacking_context: Option<&mut HoistedPaintChildren>,
    ) {
        let mut new_stacking_context: HoistedPaintChildren = HoistedPaintChildren::new();
        let stacking_context = &mut new_stacking_context;

        // Flush background/mask images from style to dedicated storage on the node
        self.flush_image_layers_from_style(node_id, ImageLayerKind::Background);
        self.flush_image_layers_from_style(node_id, ImageLayerKind::Mask);

        let incremental = self.incremental_layout;

        // Skip an untouched subtree outright, rather than walking it to find
        // out it is untouched.
        //
        // `propagate_damage_flags` stores the union of a node's own damage and
        // its whole subtree's, so an empty value means nothing under here
        // changed. That was already enough to skip rebuilding the taffy style,
        // and not enough to skip the recursion, because an ancestor rebuilds
        // its stacking context from scratch and a subtree that feeds it would
        // vanish from paint. `subtree_hoists` is that missing bit, set below
        // while walking.
        //
        // It is the largest phase in an idle frame: at 7,008 nodes a frame
        // that recomputes nothing still spent 3.5ms here, walking the tree to
        // discover it had nothing to do.
        // A node that has never had a taffy style built is not "untouched", it
        // is unbuilt, and skipping it leaves layout running against defaults:
        // no `min-width: 0`, no flex, so a revealed pane laid out at its
        // max-content width of 150,948px. Nothing distinguished the two cases
        // while stylo discarded the styles of a hidden subtree, because a
        // subtree that had never been flushed had never been styled either.
        let never_flushed = self
            .nodes
            .get(node_id)
            .is_some_and(|node| node.style_source_opt().is_none());

        // A paint-only restyle can replace the computed-values arc without
        // adding layout damage. The cached taffy style may contain raw calc()
        // pointers into that arc, so arc identity is also part of the subtree
        // skip condition.
        let style_changed = {
            let node = &self.nodes[node_id];
            let stylo_element_data = node.stylo_element_data_opt().and_then(|s| s.get());
            let primary = stylo_element_data
                .as_ref()
                .and_then(|data| data.styles.get_primary());
            match (primary, node.style_source_opt()) {
                (Some(current), Some(cached)) => !ServoArc::ptr_eq(current, cached),
                (None, None) => false,
                _ => true,
            }
        };

        if incremental
            && subtree_skip_enabled()
            && !never_flushed
            && !style_changed
            && self
                .nodes
                .get(node_id)
                .and_then(|node| node.damage())
                .is_some_and(|damage| damage.is_empty())
            && !self.nodes[node_id].subtree_hoists()
        {
            return;
        }

        let display = {
            let node = self.nodes.get_mut(node_id).unwrap();
            let damage = node.damage().unwrap_or(ALL_DAMAGE);

            // Only rebuild the taffy style when something asked for it.
            //
            // `propagate_damage_flags` stores the union of a node's own damage
            // and its whole subtree's, so an empty value here means nothing
            // under this node changed and last pass's taffy style is still
            // correct. Recomputing it anyway is what made a steady-state frame
            // — one where the page is laid out and only an animation is
            // running — cost a full `to_taffy_style` for every node in the
            // document, thirty times a second.
            //
            // Only in incremental mode. Without it `propagate_damage_flags`
            // never runs, so the damage read above is whatever was last left
            // on the node, and gating on it would skip real work.
            //
            // The recursion below is deliberately *not* gated. A node that
            // contributes hoisted children to an ancestor's stacking context
            // has to walk even when unchanged, because the ancestor rebuilds
            // that list from scratch and would otherwise lose them.
            // Damage alone is not a safe gate, because the taffy style borrows
            // from the computed values rather than owning them: a `calc()`
            // reaches taffy as a raw pointer into the stylo
            // `CalcLengthPercentage` (see `stylo_taffy::convert`). A restyle
            // that lands no relayout damage — a colour change, or one that
            // computes to the same values — still replaces the primary
            // `ComputedValues`, and if that drops the last reference the cached
            // pointer is dangling. Layout then resolves freed memory, which is
            // a segfault when the page is unmapped and a nonsense calc node
            // when it is not: 0.6.x experimental died both ways, seconds after
            // boot, whenever a slow command's response restyled the header.
            //
            // So rebuild whenever the arc is not the one the cached style was
            // built from. Identity, not equality: a fresh arc means fresh
            // allocations behind every pointer in the old style. The steady
            // state this gate exists for is unaffected, because a frame that
            // restyles nothing hands back the same arc.
            let needs_style_flush = !incremental
                || style_changed
                || damage.intersects(RestyleDamage::RELAYOUT | CONSTRUCT_BOX);

            if needs_style_flush {
                // Compute the owned taffy style and display in an inner scope so the
                // immutable borrow of `node` (held by the stylo element data guard)
                // is released before we mutably access `node` below.
                let (mut taffy_style, display_constructed_as, style_source) = {
                    let stylo_element_data = node.stylo_element_data_opt().and_then(|s| s.get());
                    let primary_styles = stylo_element_data
                        .as_ref()
                        .and_then(|data| data.styles.get_primary());

                    let Some(style) = primary_styles else {
                        return;
                    };

                    (
                        stylo_taffy::to_taffy_style(style),
                        style.clone_display(),
                        style.clone(),
                    )
                };
                taffy_style.item_is_replaced = node
                    .data
                    .downcast_element()
                    .is_some_and(|el| crate::layout::replaced::is_replaced_element(&el.name.local));

                // A rebuilt style and a retained layout cache have to agree.
                // The cache is cleared by `propagate_damage_flags` only for
                // relayout damage, so a style refreshed for any other reason
                // leaves this node answering from a cache computed against the
                // values it just replaced, while its parent lays out against
                // the new ones. Comparing is cheap next to laying out, and
                // equal styles are the common case: a recolour rebuilds an
                // identical taffy style and keeps its cache.
                let layout_inputs_changed = *node.style() != taffy_style;
                *node.style_mut() = taffy_style;
                *node.display_constructed_as_mut() = display_constructed_as;
                if layout_inputs_changed {
                    node.cache_mut().clear();
                    if let Some(inline_layout) = node
                        .data
                        .downcast_element_mut()
                        .and_then(|el| el.inline_layout_data.as_mut())
                    {
                        inline_layout.content_widths = None;
                    }
                }
                // Stored last, and only on the path that rebuilt the style, so
                // the arc held here is always the one the pointers in
                // `node.style()` point into. It keeps those allocations alive
                // for as long as the style that borrows them.
                *node.style_source_mut() = Some(style_source);
            } else if node
                .stylo_element_data_opt()
                .and_then(|s| s.get())
                .as_ref()
                .and_then(|data| data.styles.get_primary())
                .is_none()
            {
                // Preserved from the ungated form: a node with no primary style
                // is not laid out and its subtree is not walked.
                return;
            }

            // In non-incremental mode we unconditionally clear the Taffy cache.
            // In incremental mode this is handled as part of damage propagation.
            if !incremental {
                node.cache_mut().clear();
                if let Some(inline_layout) = node
                    .data
                    .downcast_element_mut()
                    .and_then(|el| el.inline_layout_data.as_mut())
                {
                    inline_layout.content_widths = None;
                }
            }

            node.style().display
        };

        // A hidden subtree is not walked.
        //
        // Its taffy style is flushed above, which is what lets paint stop at
        // this node — but only for children it reaches *through* this node. A
        // positioned child with a z-index is hoisted into an ancestor's
        // stacking context and painted from there, so it never passes this
        // node's display check at all. Walking a hidden subtree therefore
        // publishes its raised children into the visible tab's paint list: the
        // application's panel-edge chevron is `absolute left-full z-20`, and
        // one ghost chevron appeared per retained tab.
        //
        // This walk could not reach a hidden subtree before, because hiding a
        // pane emptied its layout children and stylo discarded its styles.
        if matches!(display, taffy::Display::None) {
            return;
        }

        // Hoisted fixed nodes, held back until the borrow on paint_children is
        // released so their real stacking context can be reached.
        let mut deferred_fixed: Vec<(NodeId, i32, NodeId)> = Vec::new();

        // If the node has children, then take those children and...
        let children = self.nodes[node_id].layout_children.borrow_mut().take();
        if let Some(mut children) = children {
            let is_flex_or_grid = matches!(display, taffy::Display::Flex | taffy::Display::Grid);

            // Recursively call flush_styles_to_layout on each child
            for &child in children.iter() {
                self.flush_styles_to_layout_impl(
                    child,
                    match self.nodes[child].is_stacking_context_root(is_flex_or_grid) {
                        true => None,
                        false => Some(stacking_context),
                    },
                );
            }

            // Sort layout_children
            if is_flex_or_grid {
                children.sort_by(|left, right| {
                    let left_node = self.nodes.get(*left).unwrap();
                    let right_node = self.nodes.get(*right).unwrap();
                    left_node.order().cmp(&right_node.order())
                });
            }

            // Reserve space for paint_children
            let mut paint_children = self.nodes[node_id].paint_children.borrow_mut();
            if paint_children.is_none() {
                *paint_children = Some(ThinVec::new());
            }
            let paint_children = paint_children.as_mut().unwrap();
            paint_children.clear();
            paint_children.reserve(children.len());

            // Push children to either paint_children or layout_children depending on
            for &child_id in children.iter() {
                let child = &self.nodes[child_id];

                let Some(style) = child.primary_styles() else {
                    paint_children.push(child_id);
                    continue;
                };

                let position = style.clone_position();
                let z_index = style.clone_z_index().integer_or(0);

                // TODO: more complete hoisting detection
                // z-index applies to static flex/grid items too
                // (css-flexbox-1 §painting, css-grid-1 §z-order).
                if z_index != 0 && (position != Position::Static || is_flex_or_grid) {
                    // A hoisted fixed node paints in the stacking context its
                    // box tree gives it, not the one the hoist moved it to.
                    // `hoist_fixed_position_nodes` reparents it onto the root
                    // element so its insets resolve against the viewport, which
                    // is what CSS asks for; the stacking context is a separate
                    // question and follows the original ancestors.
                    if let Some(&origin) = self.hoisted_fixed_parents.get(&child_id) {
                        deferred_fixed.push((child_id, z_index, origin));
                    } else {
                        stacking_context
                            .children
                            .push(HoistedPaintChild::new(child_id, z_index, position))
                    }
                } else {
                    paint_children.push(child_id);
                }
            }

            // Sort paint_children
            paint_children.sort_by(|left, right| {
                let left_node = self.nodes.get(*left).unwrap();
                let right_node = self.nodes.get(*right).unwrap();
                node_to_paint_order(left_node, is_flex_or_grid)
                    .cmp(&node_to_paint_order(right_node, is_flex_or_grid))
            });

            // Put children back
            *self.nodes[node_id].layout_children.borrow_mut() = Some(children);
        }

        // Outside the block above, so the borrow on paint_children has ended:
        // reaching another node's stacking context needs `self` mutably.
        let hoisted_fixed_here = !deferred_fixed.is_empty();
        for (child_id, z_index, origin) in deferred_fixed {
            self.place_hoisted_fixed(child_id, z_index, origin, node_id, stacking_context);
        }

        // Anything this subtree contributes upward makes it unskippable next
        // frame. A hoisted fixed node counts even when this node establishes a
        // stacking context, because `place_hoisted_fixed` reaches a context
        // that is not this one.
        let feeds_an_ancestor = hoisted_fixed_here
            || (parent_stacking_context.is_some() && !stacking_context.children.is_empty());
        *self.nodes[node_id].subtree_hoists_mut() = feeds_an_ancestor;

        if let Some(parent_stacking_context) = parent_stacking_context {
            let position = self.nodes[node_id].final_layout().location;
            let scroll_offset = *self.nodes[node_id].scroll_offset();

            // Everything below is leaving this node's subtree, so this node's
            // own clip layer will not contain any of it. Note the clip here and
            // let it ride up with the children it applies to.
            let (clips_here, is_containing_block) = self.nodes[node_id]
                .primary_styles()
                .map(|styles| {
                    let box_styles = styles.get_box();
                    (
                        !matches!(box_styles.overflow_x, StyloOverflow::Visible)
                            || !matches!(box_styles.overflow_y, StyloOverflow::Visible),
                        // A positioned box is a containing block for absolutely
                        // positioned descendants. A transform or a filter makes
                        // one too, but both make a stacking context as well,
                        // which stops the hoist before it reaches here.
                        box_styles.position != Position::Static,
                    )
                })
                .unwrap_or((false, false));

            for hoisted in stacking_context.children.iter_mut() {
                // Before the push, because a containing block clips its own
                // absolutely positioned children.
                if hoisted.starts_at_containing_block && is_containing_block {
                    hoisted.clips_apply = true;
                }
                if clips_here && hoisted.clips_apply {
                    hoisted.clip_ancestors.push(node_id);
                }

                hoisted.position.x += position.x - scroll_offset.x as f32;
                hoisted.position.y += position.y - scroll_offset.y as f32;
            }
            parent_stacking_context
                .children
                .extend(stacking_context.children.iter().cloned());
        } else {
            stacking_context.sort();
            stacking_context.compute_content_size(self);
            if stacking_context
                .children
                .iter()
                .any(|child| !child.clip_ancestors.is_empty())
            {
                self.hoisted_clip_hosts.push(node_id);
            }
            self.nodes[node_id].stacking_context = Some(Box::new(new_stacking_context));
        }
    }
}

impl BaseDocument {
    /// Put a hoisted `position: fixed` node into the stacking context its box
    /// tree gives it, rather than the root's.
    ///
    /// `origin` is the layout parent the node was taken from. Walking up from
    /// there finds the nearest ancestor that establishes a stacking context,
    /// which is where CSS says the node paints. When that ancestor is the node
    /// we are already building a context for, the caller's context is it and
    /// nothing special is needed.
    ///
    /// Descendants are flushed before their parent's hoisting pass, so an
    /// ancestor's context is already built and sorted by the time this runs.
    /// Pushing into it means sorting it again.
    ///
    /// The offset that compensates for the move is filled in later, by
    /// `correct_hoisted_fixed_positions`, because layout does not exist yet
    /// when this runs.
    fn place_hoisted_fixed(
        &mut self,
        child_id: NodeId,
        z_index: i32,
        origin: NodeId,
        current: NodeId,
        current_context: &mut HoistedPaintChildren,
    ) {
        let host = self.nearest_stacking_context_ancestor(origin);

        if host == Some(current) || host.is_none() {
            current_context.children.push(HoistedPaintChild::new(
                child_id,
                z_index,
                Position::Fixed,
            ));
            return;
        }
        let host = host.unwrap();

        // The offset cannot be computed here: this runs before taffy has laid
        // anything out, so every absolute position is still zero. It is filled
        // in by `correct_hoisted_fixed_positions` once layout exists.
        let position = taffy::Point::ZERO;

        let Some(context) = self.nodes[host].stacking_context.as_mut() else {
            // No context to join. Falling back to the caller's keeps the node
            // painted rather than dropping it.
            current_context.children.push(HoistedPaintChild::new(
                child_id,
                z_index,
                Position::Fixed,
            ));
            return;
        };
        let mut hoisted = HoistedPaintChild::new(child_id, z_index, Position::Fixed);
        hoisted.position = position;
        context.children.push(hoisted);
        let mut context = self.nodes[host].stacking_context.take().unwrap();
        context.sort();
        context.compute_content_size(self);
        self.nodes[host].stacking_context = Some(context);

        // This context was mutated after its subtree had already been walked.
        // Keep the path to the host out of the untouched-subtree fast path on
        // the next flush so the host rebuilds the list before this fixed child
        // is placed into it again. Marking only `host` is not enough: an
        // unchanged ancestor would return before traversal ever reached it.
        //
        // Without the rebuild every idle resolve appends another copy of the
        // child, and a box shadow darkens once per pointer-driven redraw. The
        // rebuild also removes the retained entry after the fixed node leaves.
        let mut current = Some(host);
        while let Some(node_id) = current {
            current = self.nodes[node_id].layout_parent.get();
            *self.nodes[node_id].subtree_hoists_mut() = true;
        }
    }

    /// The nearest ancestor of `node_id`, inclusive, that establishes a
    /// stacking context.
    pub(crate) fn nearest_stacking_context_ancestor(&self, node_id: NodeId) -> Option<NodeId> {
        let mut current = Some(node_id);
        while let Some(id) = current {
            let node = self.nodes.get(id)?;
            let is_flex_or_grid_item = node
                .layout_parent
                .get()
                .and_then(|parent| self.nodes.get(parent))
                .is_some_and(|parent| {
                    matches!(
                        parent.style().display,
                        taffy::Display::Flex | taffy::Display::Grid
                    )
                });
            if node.is_stacking_context_root(is_flex_or_grid_item) {
                return Some(id);
            }
            current = node.layout_parent.get();
        }
        None
    }
}

#[inline(always)]
fn position_to_order(pos: Position) -> i32 {
    match pos {
        Position::Static => 0,
        // All positioned descendants with z-index: auto share one paint
        // level (CSS 2.1 Appendix E step 8); the stable sort keeps them in
        // tree order among themselves, above in-flow content and floats.
        Position::Relative | Position::Sticky | Position::Absolute | Position::Fixed => 2,
    }
}
#[inline(always)]
fn float_to_order(pos: Float) -> i32 {
    match pos {
        Float::None => 0,
        _ => 1,
    }
}

/// Paint sort key: (paint level, order-modified position). Positioned
/// (z-index: auto) descendants paint above in-flow content (CSS 2.1
/// Appendix E step 8); within a level the stable sort preserves
/// (order-modified) document order.
#[inline(always)]
fn node_to_paint_order(node: &Node, is_flex_or_grid: bool) -> (i32, i32) {
    let Some(style) = node.primary_styles() else {
        return (0, 0);
    };
    let position = style.clone_position();
    if is_flex_or_grid {
        match position {
            Position::Static => (0, style.clone_order()),
            Position::Relative | Position::Sticky => (2, style.clone_order()),
            // Out-of-flow children are not flex/grid items: `order` does
            // not apply; tree order does.
            Position::Absolute | Position::Fixed => (2, 0),
        }
    } else {
        (
            position_to_order(position) + float_to_order(style.clone_float()),
            0,
        )
    }
}

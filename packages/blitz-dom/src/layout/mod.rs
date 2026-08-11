//! Enable the dom to lay itself out using taffy
//!
//! In servo, style and layout happen together during traversal
//! However, in Blitz, we do a style pass then a layout pass.
//! This is slower, yes, but happens fast enough that it's not a huge issue.

use crate::node::{ImageData, NodeData, SpecialElementData};
use crate::{document::BaseDocument, dom_node_id, node::Node, taffy_node_id};
use markup5ever::local_name;
use std::cell::Ref;
use std::sync::Arc;
use style::Atom;
use style::values::computed::CSSPixelLength;
use style::values::computed::length_percentage::CalcLengthPercentage;
use taffy::{
    BlockContext, CollapsibleMarginSet, FlexDirection, LayoutPartialTree, MaybeResolve, NodeId,
    ResolveOrZero, RoundTree, Style, TraversePartialTree, TraverseTree, compute_block_layout,
    compute_cached_layout, compute_flexbox_layout, compute_grid_layout, compute_leaf_layout,
    prelude::*,
};

/// How much of the tree a single resolve actually recomputed.
///
/// Phase timings say layout is expensive; they cannot say whether that is a
/// handful of slow nodes or the whole tree missing its cache. These counters
/// answer that, and a wrong answer sends the fix to the wrong place entirely.
/// Thread-local and read once per resolve, so the counting itself is free.
#[cfg(feature = "log-phase-times")]
pub(crate) mod layout_counters {
    use blitz_traits::node_id::NodeId;
    use std::cell::Cell;

    thread_local! {
        static COMPUTED: Cell<u64> = const { Cell::new(0) };
        static CACHES_CLEARED: Cell<u64> = const { Cell::new(0) };
        static LOOKUPS: Cell<u64> = const { Cell::new(0) };
        static HITS: Cell<u64> = const { Cell::new(0) };
        /// Distinct nodes recomputed, to tell "the whole tree once" apart from
        /// "a few nodes many times". Those have completely different fixes and
        /// the totals alone cannot distinguish them.
        static DISTINCT: std::cell::RefCell<std::collections::HashMap<NodeId, u32>> =
            std::cell::RefCell::new(std::collections::HashMap::new());
    }

    pub(crate) fn note_computed(node_id: NodeId) {
        COMPUTED.with(|count| count.set(count.get() + 1));
        DISTINCT.with(|seen| {
            *seen.borrow_mut().entry(node_id).or_insert(0u32) += 1;
        });
    }

    /// The nodes recomputed most often, worst first.
    ///
    /// Totals say the work is concentrated; only the identities say where. A
    /// node recomputed a hundred times is either being measured under a hundred
    /// different constraints or sitting under a container that re-descends, and
    /// naming it is the difference between fixing that and guessing again.
    pub(crate) fn worst_offenders(limit: usize) -> Vec<(NodeId, u32)> {
        DISTINCT.with(|seen| {
            let mut rows: Vec<(NodeId, u32)> = seen
                .borrow()
                .iter()
                .map(|(id, count)| (*id, *count))
                .collect();
            rows.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
            rows.truncate(limit);
            rows
        })
    }

    pub(crate) fn note_cache_cleared() {
        CACHES_CLEARED.with(|count| count.set(count.get() + 1));
    }

    pub(crate) fn note_lookup(hit: bool) {
        LOOKUPS.with(|count| count.set(count.get() + 1));
        if hit {
            HITS.with(|count| count.set(count.get() + 1));
        }
    }

    pub(crate) struct LayoutCounts {
        pub computed: u64,
        pub distinct: usize,
        pub caches_cleared: u64,
        pub lookups: u64,
        pub hits: u64,
    }

    /// Counts since the last call, then reset.
    pub(crate) fn take() -> LayoutCounts {
        LayoutCounts {
            computed: COMPUTED.with(|count| count.replace(0)),
            distinct: DISTINCT.with(|seen| {
                let mut seen = seen.borrow_mut();
                let len = seen.len();
                seen.clear();
                len
            }),
            caches_cleared: CACHES_CLEARED.with(|count| count.replace(0)),
            lookups: LOOKUPS.with(|count| count.replace(0)),
            hits: HITS.with(|count| count.replace(0)),
        }
    }
}

pub(crate) mod construct;
pub(crate) mod damage;
pub(crate) mod inline;
pub(crate) mod list;
pub(crate) mod replaced;
pub(crate) mod table;

use self::replaced::{ReplacedContext, is_replaced_element, replaced_measure_function};
use self::table::TableTreeWrapper;

pub(crate) fn resolve_calc_value(calc_ptr: *const (), parent_size: f32) -> f32 {
    let calc = unsafe { &*(calc_ptr as *const CalcLengthPercentage) };
    let result = calc.resolve(CSSPixelLength::new(parent_size));
    result.px()
}

impl BaseDocument {
    fn node_from_id(&self, node_id: taffy::prelude::NodeId) -> &Node {
        &self.nodes[dom_node_id(node_id)]
    }
    fn node_from_id_mut(&mut self, node_id: taffy::prelude::NodeId) -> &mut Node {
        &mut self.nodes[dom_node_id(node_id)]
    }
}

impl BaseDocument {
    fn compute_child_layout_internal(
        &mut self,
        node_id: NodeId,
        inputs: taffy::tree::LayoutInput,
        block_ctx: Option<&mut BlockContext<'_>>,
    ) -> taffy::tree::LayoutOutput {
        // Counted, not timed. The layout phase dominates a script-forced
        // resolve, and the two explanations (a few nodes that are each slow, or
        // the whole tree recomputing) call for opposite fixes. Only the blast
        // radius separates them, and a cache hit never reaches this function.
        #[cfg(feature = "log-phase-times")]
        layout_counters::note_computed(dom_node_id(node_id));
        let node = &mut self.nodes[dom_node_id(node_id)];

        let font_styles = node.primary_styles().map(|style| {
            use style::values::computed::font::LineHeight;

            let font_size = style.clone_font_size().used_size().px();
            let line_height = match style.clone_line_height() {
                LineHeight::Normal => font_size * 1.2,
                LineHeight::Number(num) => font_size * num.0,
                LineHeight::Length(value) => value.0.px(),
            };

            (font_size, line_height)
        });
        let font_size = font_styles.map(|s| s.0);
        let resolved_line_height = font_styles.map(|s| s.1);

        match &mut node.data {
            NodeData::Text(data) => {
                // With the new "inline context" architecture all text nodes should be wrapped in an "inline layout context"
                // and should therefore never be measured individually.
                #[cfg(feature = "tracing")]
                tracing::error!(
                    node_id = ?dom_node_id(node_id),
                    data = ?data,
                    "Tried to lay out text node individually",
                );

                #[cfg(not(feature = "tracing"))]
                let _ = data;

                taffy::LayoutOutput::HIDDEN
                // unreachable!();

                // compute_leaf_layout(inputs, &node.style, |known_dimensions, available_space| {
                //     let context = TextContext {
                //         text_content: &data.content.trim(),
                //         writing_mode: WritingMode::Horizontal,
                //     };
                //     let font_metrics = FontMetrics {
                //         char_width: 8.0,
                //         char_height: 16.0,
                //     };
                //     text_measure_function(
                //         known_dimensions,
                //         available_space,
                //         &context,
                //         &font_metrics,
                //     )
                // })
            }
            NodeData::Element(element_data) | NodeData::AnonymousBlock(element_data) => {
                // TODO: deduplicate with single-line text input
                if *element_data.name.local == *"textarea" {
                    let rows = element_data
                        .attr(local_name!("rows"))
                        .and_then(|val| val.parse::<f32>().ok())
                        .unwrap_or(2.0);

                    let cols = element_data
                        .attr(local_name!("cols"))
                        .and_then(|val| val.parse::<f32>().ok());

                    let intrinsic_height = resolved_line_height.unwrap_or(16.0) * rows;

                    // Give the editor the width it has to lay out within, so a
                    // long line wraps instead of running off the side. Without
                    // this the editor is built with `set_width(None)` and never
                    // told otherwise: `wrap="soft"` and `overflow-wrap` in the
                    // stylesheet have nothing to act on, and typing past the
                    // right edge walks the text out of the box and out of sight.
                    //
                    // The node's own `width` comes first. `known_dimensions` is
                    // what the parent has decided so far and does not yet
                    // include this element's style size, so reading only that
                    // hands the editor the parent's width and it wraps, when it
                    // wraps at all, to the wrong measure.
                    let content_width = node
                        .style()
                        .size
                        .width
                        .maybe_resolve(inputs.parent_size.width, resolve_calc_value)
                        .or(inputs.known_dimensions.width)
                        .or(match inputs.available_space.width {
                            taffy::AvailableSpace::Definite(width) => Some(width),
                            _ => None,
                        })
                        .map(|width| {
                            let inset = node
                                .style()
                                .padding
                                .resolve_or_zero(inputs.parent_size, resolve_calc_value)
                                .horizontal_components()
                                .sum()
                                + node
                                    .style()
                                    .border
                                    .resolve_or_zero(inputs.parent_size, resolve_calc_value)
                                    .horizontal_components()
                                    .sum();
                            (width - inset).max(0.0)
                        });

                    // The wrapped text may be taller than the box. That excess
                    // is exactly what `scrollHeight` reports and what an
                    // autosizing composer grows by, so it has to reach Taffy as
                    // content size rather than be rounded away into the box
                    // height.
                    let mut content_height = intrinsic_height;
                    if let Some(width) = content_width.filter(|width| *width > 0.0) {
                        let font_ctx = self.font_ctx.clone();
                        let layout_ctx = &mut self.layout_ctx;
                        let node = &mut self.nodes[dom_node_id(node_id)];
                        if let Some(input) = node
                            .data
                            .downcast_element_mut()
                            .and_then(|el| el.text_input_data_mut())
                        {
                            input.sync_multiline_width(
                                &mut font_ctx.lock().unwrap(),
                                layout_ctx,
                                width,
                            );
                            if let Some(layout) = input.editor.try_layout() {
                                content_height = content_height.max(layout.height());
                            }
                        }
                    }

                    let node = &mut self.nodes[dom_node_id(node_id)];
                    let mut output = compute_leaf_layout(
                        inputs,
                        node.style(),
                        resolve_calc_value,
                        |_known_size, _available_space| taffy::Size {
                            width: cols
                                .map(|cols| cols * font_size.unwrap_or(16.0) * 0.6)
                                .unwrap_or(300.0),
                            height: intrinsic_height,
                        },
                    );
                    output.content_size.height = output.content_size.height.max(content_height);
                    output.content_size.width = output.content_size.width.max(output.size.width);
                    return output;
                }

                if *element_data.name.local == *"input" {
                    match element_data.attr(local_name!("type")) {
                        // if the input type is hidden, hide it
                        Some("hidden") => {
                            node.style_mut().display = Display::None;
                            return taffy::LayoutOutput::HIDDEN;
                        }
                        Some("checkbox") => {
                            return compute_leaf_layout(
                                inputs,
                                node.style(),
                                resolve_calc_value,
                                |_known_size, _available_space| {
                                    let width = node.style().size.width.resolve_or_zero(
                                        inputs.parent_size.width,
                                        resolve_calc_value,
                                    );
                                    let height = node.style().size.height.resolve_or_zero(
                                        inputs.parent_size.height,
                                        resolve_calc_value,
                                    );
                                    let min_size = width.min(height);
                                    taffy::Size {
                                        width: min_size,
                                        height: min_size,
                                    }
                                },
                            );
                        }
                        None | Some("text" | "password" | "email" | "tel" | "url" | "search") => {
                            return compute_leaf_layout(
                                inputs,
                                node.style(),
                                resolve_calc_value,
                                |_known_size, _available_space| taffy::Size {
                                    width: match inputs.available_space.width {
                                        AvailableSpace::Definite(limit) => limit.min(300.0),
                                        AvailableSpace::MinContent => 0.0,
                                        AvailableSpace::MaxContent => 300.0,
                                    },
                                    height: resolved_line_height.unwrap_or(16.0),
                                },
                            );
                        }
                        _ => {}
                    }
                }

                if is_replaced_element(&element_data.name.local) {
                    // Get width and height attributes on image element
                    //
                    // TODO: smarter sizing using these (depending on object-fit, they shouldn't
                    // necessarily just override the native size)
                    let mut attr_size = taffy::Size {
                        width: element_data
                            .attr(local_name!("width"))
                            .and_then(|val| val.parse::<f32>().ok()),
                        height: element_data
                            .attr(local_name!("height"))
                            .and_then(|val| val.parse::<f32>().ok()),
                    };

                    // Get the element's intrinsic size and aspect ratio
                    let (inherent_size, inherent_ratio) = match &element_data.special_data {
                        SpecialElementData::Image(image_data) => match &**image_data {
                            ImageData::Raster(image) => {
                                let size = taffy::Size {
                                    width: image.width as f32,
                                    height: image.height as f32,
                                };
                                (size, Some(size.width / size.height))
                            }
                            #[cfg(feature = "svg")]
                            ImageData::Svg(svg) => {
                                // For an inline `<svg>` element the width/height attributes are
                                // presentation attributes: percentages resolve against the
                                // containing block. For SVG loaded as an image the intrinsic
                                // dimensions are context-free.
                                if *element_data.name.local == local_name!("svg") {
                                    attr_size = taffy::Size {
                                        width: svg.resolved_width(inputs.parent_size.width),
                                        height: svg.resolved_height(inputs.parent_size.height),
                                    };
                                }
                                let (mut width, mut height) = svg.intrinsic_size();
                                // A replaced element with only an intrinsic aspect ratio uses the
                                // stretch-fit width in normal flow (CSS2 §10.3.2): fill the
                                // definite available width and derive the height from the ratio.
                                // Shrink-to-fit contexts (floats, abspos) keep the default object
                                // size that `intrinsic_size` already applied.
                                if svg.intrinsic_width().is_none()
                                    && svg.intrinsic_height().is_none()
                                {
                                    if let (
                                        Some(ratio),
                                        AvailableSpace::Definite(available_width),
                                    ) =
                                        (svg.viewbox_aspect_ratio(), inputs.available_space.width)
                                    {
                                        width = available_width;
                                        height = available_width / ratio;
                                    }
                                }
                                (taffy::Size { width, height }, Some(svg.aspect_ratio()))
                            }
                            ImageData::None => (taffy::Size::ZERO, None),
                        },
                        // Canvas has an intrinsic size and aspect ratio given by its
                        // width/height attributes, defaulting to 300x150. Other replaced
                        // elements without intrinsic dimensions (video, iframe, embed) use
                        // the 300x150 default object size but have no intrinsic ratio.
                        SpecialElementData::Canvas(_)
                        | SpecialElementData::SubDocument(_)
                        | SpecialElementData::None => {
                            let tag_name = &element_data.name.local;
                            if *tag_name == local_name!("img") || *tag_name == local_name!("svg") {
                                (taffy::Size::ZERO, None)
                            } else {
                                let size = taffy::Size {
                                    width: attr_size.width.unwrap_or(300.0),
                                    height: attr_size.height.unwrap_or(150.0),
                                };
                                let ratio = (*tag_name == local_name!("canvas"))
                                    .then(|| size.width / size.height);
                                (size, ratio)
                            }
                        }
                        _ => unreachable!(),
                    };

                    let replaced_context = ReplacedContext {
                        inherent_size,
                        attr_size,
                        inherent_ratio,
                    };

                    let computed = replaced_measure_function(
                        inputs.known_dimensions,
                        inputs.parent_size,
                        inputs.available_space,
                        &replaced_context,
                        node.style(),
                        inputs.sizing_mode,
                        inputs.axis,
                    );

                    return taffy::LayoutOutput {
                        size: computed,
                        content_size: computed,
                        first_baselines: taffy::Point::NONE,
                        top_margin: CollapsibleMarginSet::ZERO,
                        bottom_margin: CollapsibleMarginSet::ZERO,
                        margins_can_collapse_through: false,
                    };
                }

                if node.flags.is_table_root() {
                    let SpecialElementData::TableRoot(context) = &self.nodes[dom_node_id(node_id)]
                        .data
                        .downcast_element()
                        .unwrap()
                        .special_data
                    else {
                        panic!("Node marked as table root but doesn't have TableContext");
                    };
                    let context = Arc::clone(context);

                    let mut table_wrapper = TableTreeWrapper {
                        doc: self,
                        ctx: context,
                    };
                    let mut output = compute_grid_layout(&mut table_wrapper, node_id, inputs);

                    // HACK: Cap content size at node size to prevent scrolling
                    output.content_size.width = output.content_size.width.min(output.size.width);
                    output.content_size.height = output.content_size.height.min(output.size.height);

                    return output;
                }

                if node.flags.is_inline_root() {
                    return self.compute_inline_layout(dom_node_id(node_id), inputs, block_ctx);
                }

                // The default CSS file will set
                match node.style().display {
                    Display::Block => compute_block_layout(self, node_id, inputs, block_ctx),
                    Display::FlowRoot => compute_block_layout(self, node_id, inputs, None),
                    Display::Flex => compute_flexbox_layout(self, node_id, inputs),
                    Display::Grid => compute_grid_layout(self, node_id, inputs),
                    Display::None => taffy::LayoutOutput::HIDDEN,
                }
            }
            NodeData::Document(_) => compute_block_layout(self, node_id, inputs, None),

            _ => taffy::LayoutOutput::HIDDEN,
        }
    }
}

impl TraversePartialTree for BaseDocument {
    type ChildIter<'a> = RefCellChildIter<'a>;

    fn child_ids(&self, node_id: NodeId) -> Self::ChildIter<'_> {
        let layout_children = self.node_from_id(node_id).layout_children.borrow(); //.unwrap().as_ref();
        RefCellChildIter::new(Ref::map(layout_children, |children| {
            children.as_ref().map(|c| c.as_slice()).unwrap_or(&[])
        }))
    }

    fn child_count(&self, node_id: NodeId) -> usize {
        self.node_from_id(node_id)
            .layout_children
            .borrow()
            .as_ref()
            .map(|c| c.len())
            .unwrap_or(0)
    }

    fn get_child_id(&self, node_id: NodeId, index: usize) -> NodeId {
        taffy_node_id(
            self.node_from_id(node_id)
                .layout_children
                .borrow()
                .as_ref()
                .unwrap()[index],
        )
    }
}
impl TraverseTree for BaseDocument {}

impl LayoutPartialTree for BaseDocument {
    type CoreContainerStyle<'a>
        = &'a taffy::Style<Atom>
    where
        Self: 'a;

    type CustomIdent = Atom;

    fn get_core_container_style(&self, node_id: NodeId) -> &Style<Atom> {
        self.node_from_id(node_id).style()
    }

    fn set_unrounded_layout(&mut self, node_id: NodeId, layout: &Layout) {
        *self.node_from_id_mut(node_id).unrounded_layout_mut() = *layout;
    }

    fn resolve_calc_value(&self, calc_ptr: *const (), parent_size: f32) -> f32 {
        resolve_calc_value(calc_ptr, parent_size)
    }

    #[inline(always)]
    fn compute_child_layout(
        &mut self,
        node_id: NodeId,
        inputs: taffy::LayoutInput,
    ) -> taffy::LayoutOutput {
        compute_cached_layout(self, node_id, inputs, |tree, node_id, inputs| {
            tree.compute_child_layout_internal(node_id, inputs, None)
        })
    }
}

/// Whether laying this node out would place inline element boxes.
///
/// Plain text needs none of this: a run of text owns no child boxes, so a
/// cached layout for it is complete. Only an inline formatting context that
/// actually contains inline *elements* has positions living outside the cached
/// `LayoutOutput`.
fn node_places_inline_boxes(node: &Node) -> bool {
    node.element_data()
        .and_then(|element| element.inline_layout_data.as_ref())
        .is_some_and(|inline| !inline.layout.inline_boxes().is_empty())
}

impl taffy::CacheTree for BaseDocument {
    #[inline]
    fn cache_get(
        &self,
        node_id: NodeId,
        inputs: &taffy::LayoutInput,
    ) -> Option<taffy::LayoutOutput> {
        // Laying out an inline formatting context writes the size and position
        // of every inline *element* on its lines, as a side effect of the run.
        // A cache hit returns the size and replays none of that, so those
        // children keep the positions from whichever run last actually
        // executed, which need not be the one whose answer is being reused.
        //
        // Measured on a live transcript: twelve boxes left up to 987px past the
        // pane, an item-reference chip at x=1539 inside a block that had
        // correctly resolved to 713px and correctly wrapped to three lines. The
        // text rewrapped; the elements on it did not move. A probe inside the
        // writing loop recorded no writes at all for those nodes during the
        // session, which is what a cache hit looks like from the inside.
        //
        // Only `PerformLayout`, and only where there is an inline element to
        // misplace. Measurement hits stay cached, and so does every block, flex
        // and grid container, which is where the cache earns its keep.
        let node = self.node_from_id(node_id);
        if inputs.run_mode == taffy::RunMode::PerformLayout && node_places_inline_boxes(node) {
            #[cfg(feature = "log-phase-times")]
            layout_counters::note_lookup(false);
            return None;
        }

        let found = node.cache().get(inputs);
        #[cfg(feature = "log-phase-times")]
        layout_counters::note_lookup(found.is_some());
        found
    }

    #[inline]
    fn cache_store(
        &mut self,
        node_id: NodeId,
        inputs: &taffy::LayoutInput,
        layout_output: taffy::LayoutOutput,
    ) {
        self.node_from_id_mut(node_id)
            .cache_mut()
            .store(inputs, layout_output);
    }

    #[inline]
    fn cache_clear(&mut self, node_id: NodeId) {
        self.node_from_id_mut(node_id).cache_mut().clear();
    }
}

impl taffy::LayoutBlockContainer for BaseDocument {
    type BlockContainerStyle<'a>
        = &'a Style<Atom>
    where
        Self: 'a;

    type BlockItemStyle<'a>
        = &'a Style<Atom>
    where
        Self: 'a;

    fn get_block_container_style(&self, node_id: NodeId) -> Self::BlockContainerStyle<'_> {
        self.get_core_container_style(node_id)
    }

    fn get_block_child_style(&self, child_node_id: NodeId) -> Self::BlockItemStyle<'_> {
        self.get_core_container_style(child_node_id)
    }

    #[inline(always)]
    fn compute_block_child_layout(
        &mut self,
        node_id: NodeId,
        inputs: taffy::LayoutInput,
        block_ctx: Option<&mut BlockContext<'_>>,
    ) -> taffy::LayoutOutput {
        compute_cached_layout(self, node_id, inputs, |tree, node_id, inputs| {
            tree.compute_child_layout_internal(node_id, inputs, block_ctx)
        })
    }
}

impl taffy::LayoutFlexboxContainer for BaseDocument {
    type FlexboxContainerStyle<'a>
        = &'a Style<Atom>
    where
        Self: 'a;

    type FlexboxItemStyle<'a>
        = &'a Style<Atom>
    where
        Self: 'a;

    fn get_flexbox_container_style(&self, node_id: NodeId) -> Self::FlexboxContainerStyle<'_> {
        self.get_core_container_style(node_id)
    }

    fn get_flexbox_child_style(&self, child_node_id: NodeId) -> Self::FlexboxItemStyle<'_> {
        self.get_core_container_style(child_node_id)
    }
}

impl taffy::LayoutGridContainer for BaseDocument {
    type GridContainerStyle<'a>
        = &'a Style<Atom>
    where
        Self: 'a;

    type GridItemStyle<'a>
        = &'a Style<Atom>
    where
        Self: 'a;

    fn get_grid_container_style(&self, node_id: NodeId) -> Self::GridContainerStyle<'_> {
        self.get_core_container_style(node_id)
    }

    fn get_grid_child_style(&self, child_node_id: NodeId) -> Self::GridItemStyle<'_> {
        self.get_core_container_style(child_node_id)
    }

    fn set_detailed_grid_info(
        &mut self,
        node_id: NodeId,
        detailed_grid_info: taffy::DetailedGridInfo,
    ) {
        let node = self.node_from_id_mut(node_id);
        if let Some(element) = node.element_data_mut() {
            element.detailed_grid_info = Some(Box::new(detailed_grid_info));
        }
    }
}

impl RoundTree for BaseDocument {
    fn get_unrounded_layout(&self, node_id: NodeId) -> Layout {
        *self.node_from_id(node_id).unrounded_layout()
    }

    fn set_final_layout(&mut self, node_id: NodeId, layout: &Layout) {
        *self.node_from_id_mut(node_id).final_layout_mut() = *layout;
    }
}

impl PrintTree for BaseDocument {
    fn get_debug_label(&self, node_id: NodeId) -> &'static str {
        let node = &self.node_from_id(node_id);

        match node.data {
            NodeData::Document(_) => "DOCUMENT",
            // NodeData::Doctype { .. } => return "DOCTYPE",
            NodeData::Text { .. } => node.node_debug_str().leak(),
            NodeData::Comment { .. } => "COMMENT",
            NodeData::ShadowRoot(_) => "SHADOW ROOT",
            NodeData::AnonymousBlock(_) => "ANONYMOUS BLOCK",
            NodeData::Element(_) => {
                let style = node.style();
                let display = match style.display {
                    Display::Flex => match style.flex_direction {
                        FlexDirection::Row | FlexDirection::RowReverse => "FLEX ROW",
                        FlexDirection::Column | FlexDirection::ColumnReverse => "FLEX COL",
                    },
                    Display::Grid => "GRID",
                    Display::Block => "BLOCK",
                    Display::FlowRoot => "FLOW ROOT",
                    Display::None => "NONE",
                };
                format!("{} ({})", node.node_debug_str(), display).leak()
            } // NodeData::ProcessingInstruction { .. } => return "PROCESSING INSTRUCTION",
        }
    }

    fn get_final_layout(&self, node_id: NodeId) -> Layout {
        *self.node_from_id(node_id).final_layout()
    }
}

// pub struct ChildIter<'a>(std::slice::Iter<'a, usize>);
// impl<'a> Iterator for ChildIter<'a> {
//     type Item = NodeId;
//     fn next(&mut self) -> Option<Self::Item> {
//         self.0.next().copied().map(NodeId::from)
//     }
// }

pub struct RefCellChildIter<'a> {
    items: Ref<'a, [crate::NodeId]>,
    idx: usize,
}
impl<'a> RefCellChildIter<'a> {
    fn new(items: Ref<'a, [crate::NodeId]>) -> RefCellChildIter<'a> {
        RefCellChildIter { items, idx: 0 }
    }
}

impl Iterator for RefCellChildIter<'_> {
    type Item = NodeId;
    fn next(&mut self) -> Option<Self::Item> {
        self.items.get(self.idx).map(|id| {
            self.idx += 1;
            taffy_node_id(*id)
        })
    }
}

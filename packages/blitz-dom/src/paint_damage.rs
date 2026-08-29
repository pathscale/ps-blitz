//! What changed on screen since the previous painted frame.
//!
//! Blitz repaints the whole window every frame and has no notion of a dirty
//! region: `damage.rs` carries Stylo's `RestyleDamage`, which is an input to
//! layout, and [`resolve`](crate::BaseDocument::resolve) clears every node's
//! damage before painting starts. A painter therefore sees a document that is
//! uniformly undamaged, and asking "did anything behind this element change?"
//! has had no answer at all.
//!
//! It needs one. `backdrop-filter` costs a render pass and a blur per filtered
//! element per frame, and the only way that stops being a permanent CPU and GPU
//! cost is to skip the ones whose input did not change. A still window must
//! cost nothing, and a window where one paragraph is growing must cost nothing
//! for the panels that paragraph is not behind.
//!
//! # How the region is found
//!
//! Two halves, because two different things make a pixel change.
//!
//! Geometry, by comparison. Every node's painted box is recorded in document
//! space and compared against the one it had last frame. A box that moved,
//! resized, appeared or disappeared contributes both its old and its new
//! rectangle. That covers insertion, removal, reflow, and scrolling - a scroll
//! offset moves every descendant's absolute position, so it falls out of the
//! same comparison with nothing special written for it.
//!
//! Repaint in place, from Stylo. A colour change moves nothing, so the
//! comparison above cannot see it. Those nodes are recorded during damage
//! propagation instead, which is the one moment a node's *own* damage is
//! readable before its children's is folded into it. After propagation every
//! ancestor up to the root is marked, so a region built from damaged nodes at
//! any later point would be the whole document, every time.
//!
//! # Cost
//!
//! Off by default, because a document nobody asks this question of should not
//! pay to answer it. See
//! [`set_paint_damage_tracking`](crate::BaseDocument::set_paint_damage_tracking).
//!
//! When on it is one pass over the node list, which `resolve` already makes to
//! clear damage, plus one hash lookup and one rectangle comparison per node.
//! Absolute positions are memoised across the pass, so the whole walk is linear
//! rather than one root-ward recursion per node.

use crate::node::{Node, NodeData};
use crate::tree::NodeTree;
use blitz_traits::node_id::NodeId;
use kurbo::Rect;
use rustc_hash::{FxHashMap, FxHashSet};

/// How many separate rectangles a frame describes before it gives up on detail.
///
/// The list has to be a list. A single union rectangle spanning the top and the
/// bottom of a page covers everything between them, which reports every element
/// as changed and makes the whole mechanism a no-op that costs a walk.
///
/// Past this the frame collapses to one bounding rectangle. Conservative in the
/// only direction that is safe: a cache is dropped that could have been kept,
/// never kept when it should have been dropped.
const MAX_REGIONS: usize = 32;

/// The regions of the document whose pixels differ from the previous frame.
///
/// Coordinates are document space: CSS pixels, with ancestor scroll offsets
/// already applied, and *without* the paint scale or the viewport offset. A
/// consumer working in device pixels multiplies by the scale it painted at.
#[derive(Debug, Clone, Default)]
pub struct PaintDamage {
    /// Bumped once per resolve that found anything at all.
    ///
    /// Cheaper than the regions for the coarse question. A consumer that only
    /// wants "is this the same frame as last time" compares this and never
    /// looks at the rectangles.
    pub generation: u64,
    /// Bumped once per resolve whose final box geometry differs from the
    /// previous resolved frame.
    ///
    /// Unlike [`generation`](Self::generation), repaint-only changes such as a
    /// colour update leave this counter alone. Debug and cache consumers can
    /// therefore distinguish layout changes from paint changes without
    /// treating observation itself as a mutation.
    pub layout_generation: u64,
    /// Document-space rectangles that changed. Empty when nothing did.
    regions: Vec<Rect>,
}

impl PaintDamage {
    /// Whether anything at all changed.
    pub fn is_empty(&self) -> bool {
        self.regions.is_empty()
    }

    /// The changed rectangles, in no particular order and possibly overlapping.
    pub fn regions(&self) -> &[Rect] {
        &self.regions
    }

    /// Whether any of it lands in `region`.
    ///
    /// The question a cache asks: given the area a filter reads from, is what
    /// it read last time still valid? Touching edges do not count, for the same
    /// reason they do not in the backdrop planner: a change ending at `x1` and
    /// a read starting at `x1` share no pixel.
    pub fn intersects(&self, region: Rect) -> bool {
        self.regions.iter().any(|changed| {
            changed.x0 < region.x1
                && region.x0 < changed.x1
                && changed.y0 < region.y1
                && region.y0 < changed.y1
        })
    }

    fn add(&mut self, rect: Rect) {
        // A zero-area box paints nothing, so it cannot have changed anything.
        // Worth dropping rather than storing: display:none subtrees and
        // unstyled text nodes produce a great many of them, and every one would
        // otherwise take a slot in the bounded list below and push the frame
        // toward the collapse.
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return;
        }
        if self.regions.len() < MAX_REGIONS {
            self.regions.push(rect);
            return;
        }
        let collapsed = self
            .regions
            .iter()
            .copied()
            .fold(rect, |acc, existing| acc.union(existing));
        self.regions.clear();
        self.regions.push(collapsed);
    }
}

/// Tracks painted boxes between frames so [`PaintDamage`] can be produced.
#[derive(Debug, Default)]
pub(crate) struct PaintDamageTracker {
    enabled: bool,
    /// Every live node's painted box as of the previous captured frame.
    previous: FxHashMap<NodeId, Rect>,
    /// Nodes whose own Stylo damage was non-empty this resolve.
    ///
    /// Collected during propagation, because that is the only point at which
    /// "this node changed" is distinguishable from "something below this node
    /// changed". Deduplicated, since the repair path can propagate twice.
    repainted: FxHashSet<NodeId>,
    /// Whether propagation should still be recording into `repainted`.
    ///
    /// The line-break repair runs propagation a second time within one resolve,
    /// and by then `set_damage` has written the *propagated* value back to every
    /// node, so a second recording pass would mark every ancestor up to the root
    /// and hand back a full-frame region. Text edits are exactly what triggers
    /// the repair, so that is the common case rather than a corner.
    recording: bool,
    damage: PaintDamage,
}

impl PaintDamageTracker {
    pub(crate) fn set_enabled(&mut self, enabled: bool) {
        if self.enabled == enabled {
            return;
        }
        self.enabled = enabled;
        self.previous.clear();
        self.repainted.clear();
        self.damage = PaintDamage::default();
    }

    pub(crate) fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub(crate) fn damage(&self) -> &PaintDamage {
        &self.damage
    }

    /// Open a resolve. Anything recorded by the previous one is finished with.
    pub(crate) fn begin_resolve(&mut self) {
        if !self.enabled {
            return;
        }
        self.repainted.clear();
        self.recording = true;
        self.damage.regions.clear();
    }

    /// Stop recording repaints for the rest of this resolve.
    pub(crate) fn end_propagation(&mut self) {
        self.recording = false;
    }

    /// Note that `node_id` carried damage of its own, before its children's was
    /// folded in.
    pub(crate) fn note_own_damage(&mut self, node_id: NodeId) {
        if self.enabled && self.recording {
            self.repainted.insert(node_id);
        }
    }

    /// Compare this frame's boxes against the last, and produce the region.
    ///
    /// Runs after layout and after transforms are resolved, so the boxes are
    /// final, and before damage is cleared is not required: this half reads
    /// geometry only.
    pub(crate) fn capture(&mut self, nodes: &NodeTree) {
        if !self.enabled {
            return;
        }

        let mut origins = FxHashMap::with_capacity_and_hasher(nodes.len(), Default::default());
        let mut current = FxHashMap::with_capacity_and_hasher(nodes.len(), Default::default());
        let mut layout_changed = false;

        for (node_id, node) in nodes.iter() {
            // Text and comment nodes carry no box of their own: their ink lives
            // in an ancestor's inline layout, and asking them for a layout
            // panics rather than returning nothing. They are not skipped
            // silently - a text change still has to be damage - the repaint
            // pass below attributes them to the nearest ancestor that does have
            // a box.
            let Some(rect) = painted_box(nodes, node_id, node, &mut origins) else {
                continue;
            };
            match self.previous.remove(&node_id) {
                // Unchanged geometry. It may still have repainted in place,
                // which the second half below answers.
                Some(old) if old == rect => {}
                // Moved or resized: both the space it left and the space it
                // took are different from last frame.
                Some(old) => {
                    layout_changed = true;
                    self.damage.add(old);
                    self.damage.add(rect);
                }
                // New.
                None => {
                    layout_changed = true;
                    self.damage.add(rect);
                }
            }
            current.insert(node_id, rect);
        }

        // Whatever is left was removed from the tree. Its pixels are as changed
        // as any others, and nothing else in this pass would have seen it.
        for (_, old) in self.previous.drain() {
            layout_changed = true;
            self.damage.add(old);
        }
        self.previous = current;

        // Repaints in place: same box, different pixels. A colour change moves
        // nothing, so the comparison above cannot see it.
        let mut repainted = std::mem::take(&mut self.repainted);
        for node_id in repainted.iter() {
            if let Some(rect) = painting_ancestor(nodes, *node_id, &self.previous) {
                self.damage.add(rect);
            }
        }
        repainted.clear();
        self.repainted = repainted;

        if !self.damage.is_empty() {
            self.damage.generation = self.damage.generation.wrapping_add(1);
        }
        if layout_changed {
            self.damage.layout_generation = self.damage.layout_generation.wrapping_add(1);
        }
    }
}

/// A node's border box in document space, memoising ancestor origins.
///
/// [`Node::absolute_position`](crate::node::Node::absolute_position) walks to
/// the root on every call, which over a whole tree is one traversal per node.
/// The memo turns the same arithmetic into a single linear pass: an ancestor is
/// resolved once and every descendant reads it.
fn painted_box(
    nodes: &NodeTree,
    node_id: NodeId,
    node: &Node,
    origins: &mut FxHashMap<NodeId, (f64, f64)>,
) -> Option<Rect> {
    if !has_layout(node) {
        return None;
    }
    let (x, y) = absolute_origin(nodes, node_id, origins);
    let size = node.final_layout().size;
    Some(Rect::new(
        x,
        y,
        x + f64::from(size.width),
        y + f64::from(size.height),
    ))
}

/// Whether this node kind has a box at all.
///
/// Only element, anonymous block and document nodes do. `Node::final_layout`
/// panics for the rest rather than returning nothing, so every caller here has
/// to ask first.
fn has_layout(node: &Node) -> bool {
    matches!(
        node.data,
        NodeData::Element(_) | NodeData::AnonymousBlock(_) | NodeData::Document(_)
    )
}

/// The box a repaint of `node_id` actually lands in.
///
/// Two kinds of node have to be walked past, and both were found by a test
/// rather than by reading.
///
/// Text and comment nodes have no layout at all. That is expected: their ink
/// belongs to whichever element lays them out.
///
/// Inline elements have a layout whose size is zero, because their text is
/// placed by the parent's inline context rather than by taffy. They still
/// paint. A `<span>` recoloured or given new text reported a zero-area
/// rectangle, which is dropped as "paints nothing", so the change vanished
/// entirely - and that is the streaming-text case, the one this whole
/// mechanism exists for.
///
/// So: walk up until a box with area. The result is coarser than the change,
/// attributing a span's repaint to the block that contains it, which is the
/// safe direction. It is also the honest one: text reflowing inside a
/// fixed-size block genuinely can repaint anywhere in that block.
fn painting_ancestor(
    nodes: &NodeTree,
    node_id: NodeId,
    boxes: &FxHashMap<NodeId, Rect>,
) -> Option<Rect> {
    let mut current = node_id;
    loop {
        let node = nodes.get(current)?;
        if let Some(rect) = boxes.get(&current) {
            if rect.width() > 0.0 && rect.height() > 0.0 {
                return Some(*rect);
            }
        }
        current = node.layout_parent.get().or(node.parent)?;
    }
}

fn absolute_origin(
    nodes: &NodeTree,
    node_id: NodeId,
    origins: &mut FxHashMap<NodeId, (f64, f64)>,
) -> (f64, f64) {
    if let Some(cached) = origins.get(&node_id) {
        return *cached;
    }
    let Some(node) = nodes.get(node_id) else {
        return (0.0, 0.0);
    };
    // A boxless node contributes no offset of its own but still has to pass its
    // ancestors' through, so a caller walking up from one lands in the right
    // place rather than at the document origin.
    let (mut x, mut y) = if has_layout(node) {
        let layout = node.final_layout();
        (f64::from(layout.location.x), f64::from(layout.location.y))
    } else {
        (0.0, 0.0)
    };

    // A scroll offset moves a node's descendants, not its own border box, which
    // is why the parent's offset is subtracted here and never the node's own.
    // This mirrors `Node::absolute_position` exactly; the two disagreeing would
    // put the damage region somewhere the painter never drew.
    if let Some(parent_id) = node.layout_parent.get() {
        let (parent_x, parent_y) = absolute_origin(nodes, parent_id, origins);
        if let Some(parent) = nodes.get(parent_id) {
            let scroll = parent.scroll_offset();
            x += parent_x - scroll.x;
            y += parent_y - scroll.y;
        } else {
            x += parent_x;
            y += parent_y;
        }
    }

    origins.insert(node_id, (x, y));
    (x, y)
}

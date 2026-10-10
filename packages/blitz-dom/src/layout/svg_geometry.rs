use std::collections::{HashMap, HashSet};

use blitz_traits::events::HitResult;
use kurbo::{Affine, Point, Rect};
use usvg::svgtypes::{Align, AspectRatio, Length, LengthUnit, Transform};

use crate::document::BoundingRect;
use crate::{BaseDocument, Node, NodeId, local_name};

const SVG_NAMESPACE: &str = "http://www.w3.org/2000/svg";

pub(crate) fn is_svg(node: &Node) -> bool {
    node.element_data()
        .is_some_and(|element| element.name.ns.as_ref() == SVG_NAMESPACE)
}

fn is_graphics(node: &Node) -> bool {
    is_svg(node)
        && node.element_data().is_some_and(|element| {
            matches!(
                element.name.local.as_ref(),
                "svg"
                    | "g"
                    | "a"
                    | "path"
                    | "rect"
                    | "circle"
                    | "ellipse"
                    | "line"
                    | "polyline"
                    | "polygon"
                    | "text"
                    | "use"
                    | "image"
                    | "foreignObject"
                    | "foreignobject"
            )
        })
}

fn attr<'a>(node: &'a Node, name: &str) -> Option<&'a str> {
    node.element_data()?
        .attrs()
        .iter()
        .find(|attribute| {
            attribute.name.ns.as_ref().is_empty()
                && attribute.name.local.as_ref().eq_ignore_ascii_case(name)
        })
        .map(|attribute| attribute.value.as_ref())
}

/// IDs exist only in the image source. Existing IDs and fragment references
/// retain their values, and the live DOM is never mutated.
pub(crate) fn serialization_ids(doc: &BaseDocument, root: NodeId) -> HashMap<NodeId, String> {
    let mut used: HashSet<String> = doc
        .nodes
        .iter()
        .filter_map(|(_, node)| node.attr(local_name!("id")))
        .map(str::to_owned)
        .collect();
    let mut ids = HashMap::new();
    let mut pending = vec![root];

    while let Some(id) = pending.pop() {
        let node = &doc.nodes[id];
        if is_graphics(node) {
            let source_id = match node.attr(local_name!("id")).filter(|id| !id.is_empty()) {
                Some(id) => id.to_owned(),
                None => {
                    let mut generated =
                        format!("blitz-svg-geometry-{}-{}", root.as_u64(), id.as_u64());
                    while !used.insert(generated.clone()) {
                        generated.push('_');
                    }
                    generated
                }
            };
            ids.insert(id, source_id);
        }
        pending.extend(node.children.iter().rev().copied());
    }

    ids
}

fn rect(rect: usvg::Rect) -> Rect {
    Rect::new(
        f64::from(rect.x()),
        f64::from(rect.y()),
        f64::from(rect.right()),
        f64::from(rect.bottom()),
    )
}

fn bounding_rect(rect: Rect) -> BoundingRect {
    BoundingRect {
        x: rect.x0,
        y: rect.y0,
        width: rect.width(),
        height: rect.height(),
    }
}

fn union(bounds: &mut HashMap<NodeId, Rect>, id: NodeId, rect: Rect) {
    bounds
        .entry(id)
        .and_modify(|old| *old = old.union(rect))
        .or_insert(rect);
}

/// Immutable geometry travels with the parsed image. Only the viewport
/// transform depends on current layout, so resizing and scrolling do not
/// require parsing the SVG again.
#[derive(Debug)]
pub(crate) struct SvgGeometry {
    bounds: HashMap<NodeId, Rect>,
    bboxes: HashMap<NodeId, Rect>,
    hits: Vec<(NodeId, Rect)>,
    view_box: Option<Rect>,
    aspect: AspectRatio,
    user_to_tree: Affine,
}

impl SvgGeometry {
    pub(crate) fn build(
        doc: &BaseDocument,
        root: NodeId,
        tree: &usvg::Tree,
        ids: &HashMap<NodeId, String>,
    ) -> Self {
        let view_box = tree.intrinsic_dimensions().view_box.map(|view_box| {
            Rect::new(
                f64::from(view_box.x()),
                f64::from(view_box.y()),
                f64::from(view_box.x() + view_box.width()),
                f64::from(view_box.y() + view_box.height()),
            )
        });
        let aspect = attr(&doc.nodes[root], "preserveAspectRatio")
            .and_then(|value| value.parse().ok())
            .unwrap_or_default();
        let user_to_tree = view_box
            .map(|view_box| {
                viewport_transform(
                    view_box,
                    aspect,
                    f64::from(tree.size().width()),
                    f64::from(tree.size().height()),
                )
            })
            .unwrap_or(Affine::IDENTITY);
        let mut geometry = Self {
            bounds: HashMap::new(),
            bboxes: HashMap::new(),
            hits: Vec::new(),
            view_box,
            aspect,
            user_to_tree,
        };
        let by_id: HashMap<&str, NodeId> = ids
            .iter()
            .map(|(id, source_id)| (source_id.as_str(), *id))
            .collect();
        let mut hit_bounds = HashMap::new();

        for child in tree.root().children() {
            geometry.collect(doc, root, child, &by_id, None, false, &mut hit_bounds);
        }
        geometry.bboxes.insert(
            root,
            user_to_tree
                .inverse()
                .transform_rect_bbox(rect(tree.root().abs_bounding_box())),
        );

        // usvg does not render foreignObject contents. Its SVG viewport box
        // still has geometry, without introducing a synthetic painted shape.
        let mut pending = vec![root];
        while let Some(id) = pending.pop() {
            let node = &doc.nodes[id];
            if is_svg(node)
                && node.element_data().is_some_and(|element| {
                    element
                        .name
                        .local
                        .as_ref()
                        .eq_ignore_ascii_case("foreignObject")
                })
                && let Some(box_) = foreign_object_box(node, view_box, tree.size())
            {
                geometry.bboxes.insert(id, box_);
                let transform = user_to_tree * element_transform(doc, root, id);
                let painted = transform.transform_rect_bbox(box_);
                geometry.record(doc, root, id, painted, &mut hit_bounds);
            }

            // DOM order is paint order for the authored SVG elements.
            // Definition instances are attributed to their <use>, not to the
            // original definition's DOM nodes.
            if let Some(bounds) = hit_bounds.get(&id) {
                geometry.hits.push((id, *bounds));
            }
            pending.extend(node.children.iter().rev().copied());
        }

        geometry
    }

    fn collect(
        &mut self,
        doc: &BaseDocument,
        root: NodeId,
        node: &usvg::Node,
        ids: &HashMap<&str, NodeId>,
        inherited_owner: Option<NodeId>,
        in_use: bool,
        hit_bounds: &mut HashMap<NodeId, Rect>,
    ) {
        let mapped = (!in_use).then(|| ids.get(node.id()).copied()).flatten();
        let owner = mapped.or(inherited_owner);

        if let Some(id) = mapped {
            union(&mut self.bboxes, id, rect(node.bounding_box()));
        }

        match node {
            usvg::Node::Group(group) => {
                if group.opacity().get() == 0.0 {
                    return;
                }
                let in_use = in_use
                    || mapped.is_some_and(|id| {
                        doc.nodes[id]
                            .element_data()
                            .is_some_and(|element| element.name.local == local_name!("use"))
                    });
                for child in group.children() {
                    self.collect(doc, root, child, ids, owner, in_use, hit_bounds);
                }
            }
            usvg::Node::Path(path) if !path.is_visible() => {}
            usvg::Node::Image(image) if !image.is_visible() => {}
            _ => {
                if let Some(owner) = owner {
                    self.record(
                        doc,
                        root,
                        owner,
                        rect(node.abs_stroke_bounding_box()),
                        hit_bounds,
                    );
                }
            }
        }
    }

    fn record(
        &mut self,
        doc: &BaseDocument,
        root: NodeId,
        owner: NodeId,
        bounds: Rect,
        hit_bounds: &mut HashMap<NodeId, Rect>,
    ) {
        if !bounds.is_finite() || bounds.width() <= 0.0 || bounds.height() <= 0.0 {
            return;
        }
        union(hit_bounds, owner, bounds);
        let mut current = Some(owner);
        while let Some(id) = current {
            let node = &doc.nodes[id];
            if is_graphics(node) {
                union(&mut self.bounds, id, bounds);
            }
            if id == root {
                break;
            }
            current = node.parent;
        }
    }

    fn content_transform(&self, width: f64, height: f64) -> Affine {
        self.view_box
            .map(|view_box| {
                viewport_transform(view_box, self.aspect, width, height)
                    * self.user_to_tree.inverse()
            })
            .unwrap_or(Affine::IDENTITY)
    }
}

fn viewport_transform(view_box: Rect, aspect: AspectRatio, width: f64, height: f64) -> Affine {
    let mut sx = width / view_box.width();
    let mut sy = height / view_box.height();
    if aspect.align != Align::None {
        let scale = if aspect.slice { sx.max(sy) } else { sx.min(sy) };
        sx = scale;
        sy = scale;
    }
    let horizontal = match aspect.align {
        Align::XMidYMin | Align::XMidYMid | Align::XMidYMax => 0.5,
        Align::XMaxYMin | Align::XMaxYMid | Align::XMaxYMax => 1.0,
        _ => 0.0,
    };
    let vertical = match aspect.align {
        Align::XMinYMid | Align::XMidYMid | Align::XMaxYMid => 0.5,
        Align::XMinYMax | Align::XMidYMax | Align::XMaxYMax => 1.0,
        _ => 0.0,
    };
    Affine::translate((
        (width - view_box.width() * sx) * horizontal,
        (height - view_box.height() * sy) * vertical,
    )) * Affine::scale_non_uniform(sx, sy)
        * Affine::translate((-view_box.x0, -view_box.y0))
}

fn element_transform(doc: &BaseDocument, root: NodeId, id: NodeId) -> Affine {
    let mut transform = Affine::IDENTITY;
    let mut current = Some(id);
    while let Some(id) = current {
        if id == root {
            break;
        }
        let node = &doc.nodes[id];
        if let Some(value) = attr(node, "transform")
            && let Ok(value) = value.parse::<Transform>()
        {
            transform =
                Affine::new([value.a, value.b, value.c, value.d, value.e, value.f]) * transform;
        }
        current = node.parent;
    }
    transform
}

fn foreign_object_box(node: &Node, view_box: Option<Rect>, size: usvg::Size) -> Option<Rect> {
    let width = view_box.map_or(f64::from(size.width()), |rect| rect.width());
    let height = view_box.map_or(f64::from(size.height()), |rect| rect.height());
    let length = |name, reference, default| {
        let Some(value) = attr(node, name) else {
            return Some(default);
        };
        let value = value.parse::<Length>().ok()?;
        let factor = match value.unit {
            LengthUnit::None | LengthUnit::Px => 1.0,
            LengthUnit::Percent => reference / 100.0,
            LengthUnit::In => 96.0,
            LengthUnit::Cm => 96.0 / 2.54,
            LengthUnit::Mm => 96.0 / 25.4,
            LengthUnit::Pt => 96.0 / 72.0,
            LengthUnit::Pc => 16.0,
            _ => return None,
        };
        Some(value.number * factor)
    };
    let x = length("x", width, 0.0)?;
    let y = length("y", height, 0.0)?;
    let width = length("width", width, 0.0)?;
    let height = length("height", height, 0.0)?;
    (width > 0.0 && height > 0.0).then(|| Rect::new(x, y, x + width, y + height))
}

impl Node {
    /// Map parsed SVG canvas coordinates into this root's CSS border box.
    /// Paint uses this same transform, scaled to device pixels.
    pub fn svg_content_transform(&self) -> Option<Affine> {
        let geometry = self.element_data()?.svg_geometry.as_ref()?;
        let layout = self.final_layout();
        let left = f64::from(layout.border.left + layout.padding.left);
        let top = f64::from(layout.border.top + layout.padding.top);
        let width = f64::from(layout.size.width)
            - left
            - f64::from(layout.border.right + layout.padding.right);
        let height = f64::from(layout.size.height)
            - top
            - f64::from(layout.border.bottom + layout.padding.bottom);
        if width <= 0.0 || height <= 0.0 {
            return None;
        }
        Some(Affine::translate((left, top)) * geometry.content_transform(width, height))
    }

    pub(crate) fn hit_inline_svg(
        &self,
        x: f32,
        y: f32,
        hits: &mut Option<Vec<HitResult>>,
    ) -> Option<HitResult> {
        use style::computed_values::pointer_events::T as PointerEvents;
        use style::computed_values::visibility::T as Visibility;

        let geometry = self.element_data()?.svg_geometry.as_ref()?;
        let transform = self.svg_content_transform()?;
        let point = transform.inverse() * Point::new(f64::from(x), f64::from(y));
        for (id, bounds) in geometry.hits.iter().rev() {
            if !bounds.contains(point) {
                continue;
            }
            let node = self.tree().get(*id)?;
            if node.is_display_none()
                || node.primary_styles().is_some_and(|style| {
                    style.clone_pointer_events() == PointerEvents::None
                        || matches!(
                            style.clone_visibility(),
                            Visibility::Hidden | Visibility::Collapse
                        )
                })
            {
                continue;
            }
            let bounds = transform.transform_rect_bbox(*bounds);
            let hit = HitResult {
                node_id: *id,
                x: x - bounds.x0 as f32,
                y: y - bounds.y0 as f32,
                is_text: false,
            };
            if let Some(hits) = hits.as_mut() {
                hits.push(hit);
            } else {
                return Some(hit);
            }
        }
        None
    }
}

impl BaseDocument {
    pub(crate) fn svg_geometry_root(&self, id: NodeId) -> Option<NodeId> {
        let node = self.get_node(id)?;
        if !is_svg(node) {
            return None;
        }
        let mut current = node.parent;
        while let Some(id) = current {
            let node = self.get_node(id)?;
            if node
                .element_data()
                .is_some_and(|element| element.svg_geometry.is_some())
            {
                return Some(id);
            }
            current = node.parent;
        }
        None
    }

    pub(crate) fn svg_client_rect(&self, root: NodeId, id: NodeId) -> Option<BoundingRect> {
        let root_node = self.get_node(root)?;
        let geometry = root_node.element_data()?.svg_geometry.as_ref()?;
        let bounds = *geometry.bounds.get(&id)?;
        let mut current = Some(id);
        while let Some(id) = current {
            let node = self.get_node(id)?;
            if node.is_element() && node.is_display_none() {
                return None;
            }
            current = node.parent;
        }
        let bounds = root_node
            .svg_content_transform()?
            .transform_rect_bbox(bounds);
        let origin = root_node.absolute_position(0.0, 0.0);
        let bounds = bounding_rect(
            bounds
                + kurbo::Vec2::new(
                    f64::from(origin.x) - self.viewport_scroll.x,
                    f64::from(origin.y) - self.viewport_scroll.y,
                ),
        );
        Some(self.transformed_client_rect(root, bounds))
    }

    pub(crate) fn svg_user_bbox(&self, id: NodeId) -> Option<BoundingRect> {
        let node = self.get_node(id)?;
        if !is_graphics(node) {
            return None;
        }
        let root = if node
            .element_data()
            .is_some_and(|element| element.svg_geometry.is_some())
        {
            Some(id)
        } else {
            self.svg_geometry_root(id)
        };
        let bounds = root
            .and_then(|root| self.get_node(root))
            .and_then(Node::element_data)
            .and_then(|element| element.svg_geometry.as_ref())
            .and_then(|geometry| geometry.bboxes.get(&id))
            .copied()
            .unwrap_or(Rect::ZERO);
        Some(bounding_rect(bounds))
    }
}

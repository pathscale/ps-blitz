use blitz_traits::node_id::NodeId;
use parley::layout::PositionedLayoutItem;

use crate::BaseDocument;

impl BaseDocument {
    pub fn print_taffy_tree(&self) {
        taffy::print_tree(self, taffy::NodeId::from(0usize));
    }

    pub fn debug_log_node(&self, node_id: NodeId) {
        let node = &self.nodes[node_id];

        #[cfg(feature = "tracing")]
        {
            tracing::info!("Layout: {:?}", node.final_layout());
            tracing::info!("Style: {:?}", node.style());
        }

        println!("\nNode {} {}", node.id, node.node_debug_str());

        println!("Attrs:");

        for attr in node.attrs().into_iter().flatten() {
            println!("    {}: {}", attr.name.local, attr.value);
        }

        if node.flags.is_inline_root() {
            let inline_layout = &node
                .data
                .downcast_element()
                .unwrap()
                .inline_layout_data
                .as_ref()
                .unwrap();

            println!(
                "Size: {}x{}",
                inline_layout.layout.width(),
                inline_layout.layout.height()
            );
            println!("Text content: {:?}", inline_layout.text);
            println!("Inline Boxes:");
            for ibox in inline_layout.layout.inline_boxes() {
                print!("(id: {}) ", ibox.id);
            }
            println!();
            println!("Lines:");
            for (i, line) in inline_layout.layout.lines().enumerate() {
                let metrics = line.metrics();
                let x = metrics.inline_min_coord;
                let y = metrics.block_min_coord;
                let w = metrics.inline_max_coord - metrics.inline_min_coord;
                let h = metrics.block_max_coord - metrics.block_min_coord;
                println!("Line {i}: x:{x} y:{y} width:{w} height:{h}");
                for item in line.items() {
                    print!("  ");
                    match item {
                        PositionedLayoutItem::GlyphRun(run) => {
                            print!(
                                "RUN (x: {}, w: {}) ",
                                run.offset().round(),
                                run.run().advance()
                            )
                        }
                        PositionedLayoutItem::InlineBox(ibox) => print!(
                            "BOX {:?} (id: {} x: {} y: {} w: {}, h: {})",
                            ibox.kind,
                            ibox.id,
                            ibox.x.round(),
                            ibox.y.round(),
                            ibox.width.round(),
                            ibox.height.round()
                        ),
                    }
                    println!();
                }
            }
        }

        let layout = node.final_layout();
        println!("Layout:");
        println!(
            "  x: {x} y: {y} w: {width} h: {height} content_w: {content_width} content_h: {content_height}",
            x = layout.location.x,
            y = layout.location.y,
            width = layout.size.width,
            height = layout.size.height,
            content_width = layout.content_size.width,
            content_height = layout.content_size.height,
        );
        println!(
            "  border: l:{l} r:{r} t:{t} b:{b}",
            l = layout.border.left,
            r = layout.border.right,
            t = layout.border.top,
            b = layout.border.bottom,
        );
        println!(
            "  padding: l:{l} r:{r} t:{t} b:{b}",
            l = layout.padding.left,
            r = layout.padding.right,
            t = layout.padding.top,
            b = layout.padding.bottom,
        );
        println!(
            "  margin: l:{l} r:{r} t:{t} b:{b}",
            l = layout.margin.left,
            r = layout.margin.right,
            t = layout.margin.top,
            b = layout.margin.bottom,
        );
        println!("Parent: {:?}", node.parent);

        let children: Vec<_> = node
            .children
            .iter()
            .map(|id| &self.nodes[*id])
            .map(|node| (node.id, node.order(), node.node_debug_str()))
            .collect();
        println!("Children: {children:?}");

        println!("Layout Parent: {:?}", node.layout_parent.get());

        let layout_children: Option<Vec<_>> = node.layout_children.borrow().as_ref().map(|lc| {
            lc.iter()
                .map(|id| &self.nodes[*id])
                .map(|node| (node.id, node.order(), node.node_debug_str()))
                .collect()
        });
        if let Some(layout_children) = layout_children {
            println!("Layout Children: {layout_children:?}");
        }

        let paint_children: Option<Vec<_>> = node.paint_children.borrow().as_ref().map(|lc| {
            lc.iter()
                .map(|id| &self.nodes[*id])
                .map(|node| (node.id, node.order(), node.node_debug_str()))
                .collect()
        });
        if let Some(paint_children) = paint_children {
            println!("Paint Children: {paint_children:?}");
        }
        // taffy::print_tree(&self.dom, node_id.into());
    }
}

/// Report why the frame loop will not settle.
///
/// `is_animating()` is a single bool built from six independent sources, and
/// when it is stuck true the app renders continuously and burns CPU on a page
/// that looks idle. One bool cannot say which source is responsible, and the
/// sources have very different meanings: a running CSS animation is correct
/// and expected, a `<canvas>` is permanent by design, and a set of animations
/// belonging to elements no longer in the document is a leak.
///
/// `BLITZ_ANIMATION_DEBUG=1` prints the breakdown, rate-limited to once a
/// second so it can be left on while watching a real page.
pub(crate) fn animation_reasons_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| {
        matches!(
            std::env::var("BLITZ_ANIMATION_DEBUG").ok().as_deref(),
            Some("1") | Some("true")
        )
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn report_animation_reasons(
    doc_id: usize,
    canvas: bool,
    css_animations: bool,
    subdoc: bool,
    custom_widget: bool,
    scroll: bool,
    scrollbars: bool,
    nodes: Option<&str>,
) {
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    static LAST: Mutex<Option<Instant>> = Mutex::new(None);
    let mut last = LAST.lock().unwrap();
    let now = Instant::now();
    if last.is_some_and(|t| now.duration_since(t) < Duration::from_secs(1)) {
        return;
    }
    *last = Some(now);
    drop(last);

    let mut reasons: Vec<&str> = Vec::new();
    for (flag, name) in [
        (canvas, "canvas"),
        (css_animations, "css-animations"),
        (subdoc, "subdocument"),
        (custom_widget, "custom-widget"),
        (scroll, "scroll-animation"),
        (scrollbars, "scrollbar-fade"),
    ] {
        if flag {
            reasons.push(name);
        }
    }

    eprintln!(
        "[animating] doc={doc_id} {}{}",
        reasons.join(","),
        nodes.map(|n| format!("  {n}")).unwrap_or_default()
    );
}

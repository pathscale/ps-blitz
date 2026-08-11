use anyrender::{Filter, PaintScene};
use kurbo::{Affine, Shape};
use peniko::Mix;
use std::sync::atomic::{AtomicU32, Ordering};
use std::{cell::Cell, sync::Arc};

const LAYER_LIMIT: u32 = 1024;

/// Where a layer came from.
///
/// The first five go through [`LayerManager::maybe_with_layer`] and are subject
/// to [`LAYER_LIMIT`]. The last three push onto the scene directly and are only
/// counted here, which is the point of listing them: a total taken from the
/// managed sites alone understates the scene.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerSite {
    /// `clip-path` on the element.
    ClipPath,
    /// `opacity`, `filter` or `backdrop-filter`, clipped to the border box.
    Effect,
    /// The padding-box or content-box clip an overflowing element needs.
    Overflow,
    /// The clip around an outset box shadow.
    OutsetShadow,
    /// One per background image layer, pushed unconditionally.
    BackgroundImage,
    /// Inset box shadow. Bypasses the manager.
    InsetShadow,
    /// CSS `mask`. Bypasses the manager.
    Mask,
    /// Border painting clips. Bypass the manager.
    Border,
}

impl LayerSite {
    /// Every site, in the order they are reported.
    pub const ALL: [LayerSite; 8] = [
        LayerSite::ClipPath,
        LayerSite::Effect,
        LayerSite::Overflow,
        LayerSite::OutsetShadow,
        LayerSite::BackgroundImage,
        LayerSite::InsetShadow,
        LayerSite::Mask,
        LayerSite::Border,
    ];

    /// Short name for the frame log.
    pub fn name(self) -> &'static str {
        match self {
            LayerSite::ClipPath => "clip-path",
            LayerSite::Effect => "effect",
            LayerSite::Overflow => "overflow",
            LayerSite::OutsetShadow => "outset-shadow",
            LayerSite::BackgroundImage => "bg-image",
            LayerSite::InsetShadow => "inset-shadow",
            LayerSite::Mask => "mask",
            LayerSite::Border => "border",
        }
    }
}

/// How many layers one painted scene asked for, and what it got.
///
/// Clip and opacity layers are the part of scene complexity a renderer pays
/// most for: each one is a push, a pop and a region the rasteriser has to
/// composite separately. When `render_to_texture` inside vello is the largest
/// item in a frame, this count is the lever on it, because the submit path is
/// not the cost and the encode is charged elsewhere.
///
/// `wanted` exceeding `used` is not just a performance note. It means the
/// scene hit [`LAYER_LIMIT`] and clipping was silently skipped, so content that
/// should have been cut off was drawn in full.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SceneLayerCounts {
    /// Layers the painter asked to push.
    pub wanted: u32,
    /// Layers actually pushed. Lower than `wanted` only past the limit.
    pub used: u32,
    /// Deepest nesting reached, which bounds what the rasteriser holds at once.
    pub max_depth: u32,
    /// Layers pushed per [`LayerSite`], indexed by [`LayerSite::ALL`].
    ///
    /// These count pushes, including the three sites the manager never sees, so
    /// their sum is larger than `used` rather than a breakdown of it.
    pub by_site: [u32; 8],
}

/// Counts from the most recently painted scene.
///
/// Published unconditionally, for the same reason [`blitz_shell`'s frame log]
/// records unconditionally: a reader that has to be enabled at launch is a
/// reader a normally started app never feeds. Three relaxed stores per painted
/// scene, not per layer.
///
/// [`blitz_shell`'s frame log]: https://docs.rs/blitz-shell
static LATEST_WANTED: AtomicU32 = AtomicU32::new(0);
static LATEST_USED: AtomicU32 = AtomicU32::new(0);
static LATEST_MAX_DEPTH: AtomicU32 = AtomicU32::new(0);
#[allow(clippy::declare_interior_mutable_const)]
const ZERO: AtomicU32 = AtomicU32::new(0);
static LATEST_BY_SITE: [AtomicU32; 8] = [ZERO; 8];

/// Read the layer counts of the last scene painted in this process.
///
/// The three fields are stored separately, so a read taken while a scene is
/// being published can mix two frames. That is acceptable here and nowhere
/// worth a lock: the numbers move slowly, and a reader sampling once a second
/// is not trying to attribute a count to one specific frame.
pub fn latest_scene_layers() -> SceneLayerCounts {
    let mut by_site = [0u32; 8];
    for (slot, counter) in by_site.iter_mut().zip(LATEST_BY_SITE.iter()) {
        *slot = counter.load(Ordering::Relaxed);
    }
    SceneLayerCounts {
        wanted: LATEST_WANTED.load(Ordering::Relaxed),
        used: LATEST_USED.load(Ordering::Relaxed),
        max_depth: LATEST_MAX_DEPTH.load(Ordering::Relaxed),
        by_site,
    }
}

#[derive(Default)]
pub(crate) struct LayerManager {
    layers_used: Cell<u32>,
    layer_depth: Cell<u32>,
    layers_wanted: Cell<u32>,
    layer_depth_used: Cell<u32>,
    by_site: [Cell<u32>; 8],
}

impl LayerManager {
    /// Publish what this scene did, for [`latest_scene_layers`].
    pub(crate) fn publish(&self) {
        LATEST_WANTED.store(self.layers_wanted.get(), Ordering::Relaxed);
        LATEST_USED.store(self.layers_used.get(), Ordering::Relaxed);
        LATEST_MAX_DEPTH.store(self.layer_depth_used.get(), Ordering::Relaxed);
        for (counter, cell) in LATEST_BY_SITE.iter().zip(self.by_site.iter()) {
            counter.store(cell.get(), Ordering::Relaxed);
        }
    }

    /// Record a layer pushed straight onto the scene, bypassing this manager.
    ///
    /// Inset shadows, masks and border clips do that. They are not subject to
    /// [`LAYER_LIMIT`] and do not appear in `wanted` or `used`, so without this
    /// they would be invisible to every reading taken here.
    pub(crate) fn note_unmanaged(&self, site: LayerSite) {
        self.by_site[site as usize].update(|x| x + 1);
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn maybe_with_layer<S: PaintScene, F: FnOnce(&mut S)>(
        &self,
        scene: &mut S,
        site: LayerSite,
        condition: bool,
        opacity: f32,
        transform: Affine,
        shape: &impl Shape,
        filter: Option<Arc<Filter>>,
        backdrop_filter: Option<Arc<Filter>>,
        paint_layer: F,
    ) {
        let layer_used = self.maybe_push_layer(
            scene,
            site,
            condition,
            opacity,
            transform,
            shape,
            filter,
            backdrop_filter,
        );
        paint_layer(scene);
        self.maybe_pop_layer(scene, layer_used);
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn maybe_push_layer(
        &self,
        scene: &mut impl PaintScene,
        site: LayerSite,
        condition: bool,
        opacity: f32,
        transform: Affine,
        shape: &impl Shape,
        filter: Option<Arc<Filter>>,
        backdrop_filter: Option<Arc<Filter>>,
    ) -> bool {
        if !condition {
            return false;
        }
        self.layers_wanted.update(|x| x + 1);
        self.by_site[site as usize].update(|x| x + 1);

        // Check if clips are above limit
        let layers_available = self.layers_used.get() <= LAYER_LIMIT;
        if !layers_available {
            return false;
        }

        // Actually push the layer
        if opacity == 1.0 && filter.is_none() && backdrop_filter.is_none() {
            scene.push_clip_layer(transform, shape);
        } else {
            scene.push_layer(
                Mix::Normal,
                opacity,
                transform,
                shape,
                filter,
                backdrop_filter,
            );
        };

        // Update accounting. The high-water mark goes in its own cell: the
        // line here used to read `layer_depth.update(|x| x.max(layer_depth.get()))`,
        // which is a value maxed against itself, so the deepest nesting a scene
        // reached was never actually recorded anywhere.
        self.layers_used.update(|x| x + 1);
        self.layer_depth.update(|x| x + 1);
        self.layer_depth_used
            .update(|x| x.max(self.layer_depth.get()));

        true
    }

    pub(crate) fn maybe_pop_layer(&self, scene: &mut impl PaintScene, condition: bool) {
        if condition {
            scene.pop_layer();
            self.layer_depth.update(|x| x - 1);
        }
    }
}

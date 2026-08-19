use super::ElementCx;
use crate::color::{Color, ToColorColor as _};
use crate::layers::LayerSite;
use anyrender::PaintScene;
use kurbo::Vec2;
use peniko::{Compose, Fill, Mix};

/// Whether an alpha can change an 8-bit pixel.
///
/// Not `alpha != 0.0`. That is an exact comparison against a float that has
/// been through colour parsing, a custom property and an sRGB conversion, and
/// whether it lands on exactly zero depends on arithmetic the target chooses.
/// It held on aarch64 and did not on x86_64, so the same document pushed six
/// compositing layers on one machine and none on the other. That is a CI
/// failure which reproduces nowhere locally, and it cost most of an afternoon.
///
/// Half a unit in 8-bit is the honest threshold: below it the shadow cannot
/// move the output by one level, whatever the arithmetic did on the way.
fn is_visible_alpha(alpha: f32) -> bool {
    alpha * 255.0 >= 0.5
}

impl ElementCx<'_, '_> {
    pub(super) fn draw_outset_box_shadow(&self, scene: &mut impl PaintScene) {
        let box_shadow = &self.style.get_effects().box_shadow.0;
        let current_color = self.style.clone_color();
        /*
         * A fully transparent shadow is not a shadow. The loop below already
         * declines to draw one, but everything around it still ran: the clip
         * rect was computed and, where the element needs one, a whole
         * compositing layer was pushed and popped for a shape nothing was ever
         * painted into.
         *
         * Not a rare shape. An application driving shadow alpha from a custom
         * property has the declaration on every panel at all times and the
         * value at zero unless someone has moved a slider, so this is the
         * default state rather than an edge case: it cost one layer per panel,
         * every frame, to draw nothing.
         */
        let has_outset_shadow = box_shadow.iter().any(|shadow| {
            !shadow.inset
                && is_visible_alpha(
                    shadow
                        .base
                        .color
                        .resolve_to_absolute(&current_color)
                        .as_srgb_color()
                        .components[3],
                )
        });
        if !has_outset_shadow {
            return;
        }
        let opacity = self.style.get_effects().opacity;
        let bg_color = self
            .style
            .get_background()
            .background_color
            .resolve_to_absolute(&current_color)
            .as_srgb_color();
        let bg_is_opaque = bg_color.components[3] >= 1.0;
        let needs_clip = opacity < 1.0 || !bg_is_opaque;

        // Start from the first real outset shadow. Including Rect::ZERO pulls
        // the clip toward the scene origin, while including inset shadows can
        // make the compositing layer much larger than anything drawn here.
        // Both are especially visible for Tailwind-style rings on transparent
        // circular elements, where the oversized layer becomes an opaque
        // rectangular artifact.
        let max_shadow_rect = box_shadow
            .iter()
            .filter(|shadow| !shadow.inset)
            .map(|shadow| {
                let x = shadow.base.horizontal.px() as f64 * self.scale;
                let y = shadow.base.vertical.px() as f64 * self.scale;
                let blur = shadow.base.blur.px() as f64 * self.scale;
                let spread = shadow.spread.px() as f64 * self.scale;
                let offset = spread + blur * 2.5;

                self.frame.border_box.inflate(offset, offset) + Vec2::new(x, y)
            })
            .reduce(|prev, rect| prev.union(rect))
            .expect("outset shadow checked above");

        self.context.layer_manager.maybe_with_layer(
            scene,
            LayerSite::OutsetShadow,
            needs_clip,
            1.0,
            self.transform,
            &self.frame.shadow_clip(max_shadow_rect),
            None,
            None,
            |scene| {
                for shadow in box_shadow.iter().filter(|s| !s.inset).rev() {
                    let shadow_color = shadow
                        .base
                        .color
                        .resolve_to_absolute(&current_color)
                        .as_srgb_color();

                    let alpha = shadow_color.components[3];
                    if is_visible_alpha(alpha) {
                        let transform = self.transform.then_translate(Vec2 {
                            x: shadow.base.horizontal.px() as f64 * self.scale,
                            y: shadow.base.vertical.px() as f64 * self.scale,
                        });

                        // TODO draw shadows with matching individual radii instead of averaging
                        let radius = self.frame.border_radii.average();

                        let spread = shadow.spread.px() as f64 * self.scale;
                        let rect = self.frame.border_box.inflate(spread, spread);

                        // Fill the color
                        scene.draw_box_shadow(
                            transform,
                            rect,
                            shadow_color,
                            radius,
                            shadow.base.blur.px() as f64,
                        );
                    }
                }
            },
        )
    }

    pub(super) fn draw_inset_box_shadow(&self, scene: &mut impl PaintScene) {
        let current_color = self.style.clone_color();
        let box_shadow = &self.style.get_effects().box_shadow.0;
        /*
         * A shadow that cannot change a pixel is not drawn, and more to the
         * point costs no layers.
         *
         * The same fix as `draw_outset_box_shadow` above, for the same reason,
         * which had not reached this half. The old guard was
         * `shadow_color == Color::TRANSPARENT`, an exact comparison against a
         * float that has been through colour parsing, a custom property and an
         * sRGB conversion: see `is_visible_alpha` for why that is not reliable.
         *
         * It also sat *inside* the loop, after the padding box path had been
         * built, and used `return` rather than `continue`, so a transparent
         * shadow ahead of a visible one silently dropped the visible one.
         *
         * The layer cost is what makes this worth doing rather than tidy. Each
         * inset shadow pushes two compositing groups, every frame, and vello
         * gives every group scratch buffers that it pools by size class and
         * never releases (`ResourcePool` in `wgpu_engine.rs` has no eviction
         * path, on 0.9 or on master). A frame of AgencyZero peaked at 116
         * layers, 74 of them inset shadows, and the residue was 52 pooled 8 MB
         * blocks: 416 MB held for the life of the process. An application
         * driving shadow alpha from a custom property has the declaration on
         * every panel and the value at zero unless someone moved a slider, so
         * paying two layers per panel to draw nothing is the default state
         * rather than an edge case.
         */
        let visible = |shadow: &&style::values::computed::effects::BoxShadow| {
            shadow.inset
                && is_visible_alpha(
                    shadow
                        .base
                        .color
                        .resolve_to_absolute(&current_color)
                        .as_srgb_color()
                        .components[3],
                )
        };
        if !box_shadow.iter().any(|s| visible(&s)) {
            return;
        }

        let padding_box = self.frame.padding_box_path();

        for shadow in box_shadow.iter().filter(|s| visible(s)) {
            let shadow_color = shadow
                .base
                .color
                .resolve_to_absolute(&current_color)
                .as_srgb_color();

            // TODO draw shadows with matching individual radii instead of averaging
            let radius = self.frame.border_radii.average();
            let transform = self.transform.then_translate(Vec2 {
                x: shadow.base.horizontal.px() as f64,
                y: shadow.base.vertical.px() as f64,
            });

            // Two layers per inset shadow, neither through the manager.
            self.context
                .layer_manager
                .note_unmanaged(LayerSite::InsetShadow);
            self.context
                .layer_manager
                .note_unmanaged(LayerSite::InsetShadow);

            scene.push_layer(Mix::Normal, 1.0, self.transform, &padding_box, None, None);
            scene.fill(
                Fill::NonZero,
                self.transform,
                shadow_color,
                None,
                &padding_box,
            );

            scene.push_layer(
                Compose::DestOut,
                1.0,
                self.transform,
                &padding_box,
                None,
                None,
            );
            scene.draw_box_shadow(
                transform,
                self.frame.border_box,
                Color::WHITE,
                radius,
                shadow.base.blur.px() as f64 * self.scale,
            );

            scene.pop_layer();
            scene.pop_layer();
        }
    }
}

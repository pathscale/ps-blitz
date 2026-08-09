use super::ElementCx;
use crate::color::{Color, ToColorColor as _};
use anyrender::PaintScene;
use kurbo::Vec2;
use peniko::{Compose, Fill, Mix};

impl ElementCx<'_, '_> {
    pub(super) fn draw_outset_box_shadow(&self, scene: &mut impl PaintScene) {
        let box_shadow = &self.style.get_effects().box_shadow.0;
        let has_outset_shadow = box_shadow.iter().any(|s| !s.inset);
        if !has_outset_shadow {
            return;
        }

        let current_color = self.style.clone_color();
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
                    if alpha != 0.0 {
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
        let has_inset_shadow = box_shadow.iter().any(|s| s.inset);
        if !has_inset_shadow {
            return;
        }

        let padding_box = self.frame.padding_box_path();

        for shadow in box_shadow.iter().filter(|s| s.inset) {
            let shadow_color = shadow
                .base
                .color
                .resolve_to_absolute(&current_color)
                .as_srgb_color();
            if shadow_color == Color::TRANSPARENT {
                return;
            }

            // TODO draw shadows with matching individual radii instead of averaging
            let radius = self.frame.border_radii.average();
            let transform = self.transform.then_translate(Vec2 {
                x: shadow.base.horizontal.px() as f64,
                y: shadow.base.vertical.px() as f64,
            });

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

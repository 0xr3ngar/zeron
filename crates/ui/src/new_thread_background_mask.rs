//! A continuous paint-time source-alpha fade. Resizing changes only GPU
//! parameters, never the image pixels or atlas entry.
use gpui::{Bounds, ImageAlphaMask, Pixels, RenderImage, Window, point, px, size};
use std::sync::Arc;

fn mask(bounds: Bounds<Pixels>) -> ImageAlphaMask {
    ImageAlphaMask {
        // Place the exclusion entirely below the artwork, spanning its width.
        // The shader's distance is then purely vertical: one smoothstep from
        // full alpha at the top to zero at the bottom. A composer-shaped
        // exclusion creates a much steeper, visible contour around the input.
        bounds: Bounds::new(point(bounds.left(), bounds.bottom()), bounds.size),
        radius: px(0.0),
        feather: bounds.size.height.max(px(1.0)),
        clearance: px(0.0),
        bottom_fade: None,
    }
}

pub(crate) fn paint(source: Arc<RenderImage>, bounds: Bounds<Pixels>, window: &mut Window) {
    let width = f32::from(bounds.size.width);
    let height = f32::from(bounds.size.height);
    let source_size = source.size(0);
    if width <= 0.0 || height <= 0.0 || source_size.width.0 <= 0 || source_size.height.0 <= 0 {
        return;
    }
    let scale = (width / source_size.width.0 as f32).max(height / source_size.height.0 as f32);
    let fitted_size = size(
        px(source_size.width.0 as f32 * scale),
        px(source_size.height.0 as f32 * scale),
    );
    let fitted = Bounds::new(
        bounds.center() - point(fitted_size.width * 0.5, fitted_size.height * 0.5),
        fitted_size,
    );
    let _ = window.paint_image_fitted_masked(
        bounds,
        fitted,
        Default::default(),
        source,
        0,
        false,
        Some(mask(bounds)),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    // The radius-zero rectangle SDF and smoothstep used by ImageAlphaMask.
    fn alpha(mask: ImageAlphaMask, x: f32, y: f32) -> f32 {
        let center = mask.bounds.center();
        let dx = (x - f32::from(center.x)).abs() - f32::from(mask.bounds.size.width) * 0.5;
        let dy = (y - f32::from(center.y)).abs() - f32::from(mask.bounds.size.height) * 0.5;
        let distance = dx.max(0.0).hypot(dy.max(0.0)) + dx.max(dy).min(0.0);
        let t = ((distance - f32::from(mask.clearance)) / f32::from(mask.feather)).clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    }

    #[test]
    fn new_thread_fade_is_uniform_across_the_width_and_spans_the_full_image() {
        for (left, top, width, height) in [
            (0.0, 0.0, 1440.0, 691.2),
            (224.25, 40.5, 775.75, 489.6),
            (0.0, 0.0, 800.0, 288.0),
        ] {
            let mask = mask(Bounds::new(
                point(px(left), px(top)),
                size(px(width), px(height)),
            ));
            for x in [left, left + width * 0.25, left + width * 0.5, left + width] {
                for (fraction, expected) in [
                    (0.0, 1.0),
                    (0.25, 0.84375),
                    (0.5, 0.5),
                    (0.75, 0.15625),
                    (1.0, 0.0),
                ] {
                    assert!((alpha(mask, x, top + height * fraction) - expected).abs() < 0.0001);
                }
            }
        }
    }

    #[test]
    fn new_thread_fade_never_reappears_and_settles_gently_at_both_ends() {
        let mask = mask(Bounds::new(
            point(px(0.0), px(0.0)),
            size(px(1440.0), px(691.2)),
        ));
        let mut previous = 1.0;
        for y in 0..=692 {
            let current = alpha(mask, 720.0, y as f32);
            assert!(current <= previous);
            assert!(previous - current < 0.0022);
            previous = current;
        }
        assert!(1.0 - alpha(mask, 720.0, 1.0) < 0.00001);
        assert!(alpha(mask, 720.0, 690.2) < 0.00001);
    }
}

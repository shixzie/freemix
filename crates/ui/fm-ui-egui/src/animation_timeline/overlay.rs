//! Canvas overlay: the editor's wireframe proxy quads and onion skins drawn
//! over a preview image, so motion reads while the exact render catches up.

use egui::{Color32, Painter, Pos2, Rect, Shape, Stroke};
use fm_ui_model::animation_editor::{AnimDocument, OnionSkin, ProxyQuad};

use crate::theme::{AMBER, accent_color};

/// What [`paint_canvas_overlay`] draws.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OverlayOptions {
    /// Onion-skin frames on each side of the playhead (`0` hides them).
    pub onion_count: u32,
    /// Frames between onion skins.
    pub onion_step: u32,
    /// Whether to tint each quad with the target's animated colour.
    pub fill: bool,
}

impl Default for OverlayOptions {
    fn default() -> Self {
        Self {
            onion_count: 2,
            onion_step: 3,
            fill: true,
        }
    }
}

/// Maps canvas pixels into `preview`, which shows a `canvas` sized image
/// stretched to fill it.
#[must_use]
#[allow(clippy::cast_precision_loss)] // Canvas sizes are far below 2^24.
pub fn canvas_to_screen(preview: Rect, canvas: (u32, u32), point: [f32; 2]) -> Pos2 {
    let width = canvas.0.max(1) as f32;
    let height = canvas.1.max(1) as f32;
    Pos2::new(
        preview.min.x + point[0] / width * preview.width(),
        preview.min.y + point[1] / height * preview.height(),
    )
}

/// Draws one proxy quad: a closed outline, an optional translucent fill
/// weighted by its opacity, and a centre mark when `selected`.
pub fn paint_proxy_quad<T: PartialEq>(
    painter: &Painter,
    preview: Rect,
    canvas: (u32, u32),
    quad: &ProxyQuad<T>,
    stroke: Stroke,
    fill: bool,
    selected: bool,
) {
    let points: Vec<Pos2> = quad
        .corners
        .iter()
        .map(|corner| canvas_to_screen(preview, canvas, *corner))
        .collect();
    let fill = if fill {
        let color = quad.color;
        Color32::from_rgba_unmultiplied(
            color.r,
            color.g,
            color.b,
            alpha(f32::from(color.a) / 255.0 * quad.opacity * 0.28),
        )
    } else {
        Color32::TRANSPARENT
    };
    let stroke = Stroke::new(
        stroke.width,
        stroke.color.gamma_multiply(0.35 + 0.65 * quad.opacity),
    );
    painter.add(Shape::convex_polygon(points, fill, stroke));
    if selected {
        let center = canvas_to_screen(preview, canvas, quad.center());
        let arm = 5.0;
        painter.line_segment(
            [center - egui::vec2(arm, 0.0), center + egui::vec2(arm, 0.0)],
            stroke,
        );
        painter.line_segment(
            [center - egui::vec2(0.0, arm), center + egui::vec2(0.0, arm)],
            stroke,
        );
    }
}

/// Draws onion skins (earlier frames in the accent colour, later ones in
/// amber, fading with distance) and then the playhead's proxy quads, clipped
/// to `preview`. `selected` targets get a centre mark.
pub fn paint_canvas_overlay<T: PartialEq>(
    painter: &Painter,
    preview: Rect,
    canvas: (u32, u32),
    current: &[ProxyQuad<T>],
    skins: &[OnionSkin<T>],
    selected: &[T],
    fill: bool,
) {
    let painter = painter.with_clip_rect(preview);
    let farthest = skins
        .iter()
        .map(|skin| skin.offset.unsigned_abs())
        .max()
        .unwrap_or(1)
        .max(1);
    for skin in skins {
        let base = if skin.offset < 0 {
            accent_color()
        } else {
            AMBER
        };
        #[allow(clippy::cast_precision_loss)] // Offsets are a handful of frames.
        let nearness = 1.0 - (skin.offset.unsigned_abs() as f32 / (farthest as f32 + 1.0));
        let stroke = Stroke::new(1.0, base.gamma_multiply(0.15 + 0.45 * nearness));
        for quad in &skin.quads {
            paint_proxy_quad(&painter, preview, canvas, quad, stroke, false, false);
        }
    }
    for quad in current {
        let is_selected = selected.contains(&quad.target);
        let stroke = Stroke::new(
            if is_selected { 2.0 } else { 1.5 },
            if is_selected {
                accent_color()
            } else {
                Color32::WHITE
            },
        );
        paint_proxy_quad(&painter, preview, canvas, quad, stroke, fill, is_selected);
    }
}

/// [`paint_canvas_overlay`] straight from a document: the proxy at the
/// playhead and `options.onion_count` skins each side, marking targets that
/// have selected keys.
pub fn paint_document_overlay<T: Copy + Ord>(
    painter: &Painter,
    preview: Rect,
    document: &AnimDocument<T>,
    options: OverlayOptions,
) {
    let skins = if options.onion_count == 0 {
        Vec::new()
    } else {
        document.onion_skins(options.onion_count, options.onion_step)
    };
    let mut selected: Vec<T> = document
        .selection()
        .iter()
        .map(|key| key.track.target)
        .collect();
    selected.dedup();
    paint_canvas_overlay(
        painter,
        preview,
        document.canvas(),
        &document.proxy_at_playhead(),
        &skins,
        &selected,
        options.fill,
    );
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Clamped to 0..=255.
fn alpha(fraction: f32) -> u8 {
    (fraction.clamp(0.0, 1.0) * 255.0).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canvas_points_map_linearly_into_the_preview_rect() {
        let preview = Rect::from_min_size(Pos2::new(10.0, 20.0), egui::vec2(192.0, 108.0));
        let mapped = canvas_to_screen(preview, (1_920, 1_080), [960.0, 540.0]);
        assert!((mapped.x - 106.0).abs() < 1e-3 && (mapped.y - 74.0).abs() < 1e-3);
        assert_eq!(
            canvas_to_screen(preview, (1_920, 1_080), [0.0, 0.0]),
            preview.min
        );
    }
}

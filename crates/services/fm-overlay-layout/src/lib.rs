//! Downstream overlay geometry shared by the native renderer and the
//! simulated monitor tap.
//!
//! A positioned overlay covers a third of the canvas in each axis, inset from
//! the chosen corner by a margin of `min(width, height) / 32` pixels (at least
//! one); a full-frame overlay covers the whole canvas. The inset white border
//! scales with the overlay's shorter side. Both functions are pure, so the
//! same preset lands on the same pixels wherever it is evaluated.

use fm_compositor::{Rotation, Transform};
use fm_switcher::{OverlayBorderPreset, OverlayPositionPreset};
use fm_types::VideoDimensions;

/// The overlay layer transform for `position` on a `dimensions` canvas.
///
/// Returns `None` only when a coordinate does not fit the compositor's signed
/// translation, which no valid video dimension reaches.
#[must_use]
pub fn overlay_transform(
    dimensions: VideoDimensions,
    position: OverlayPositionPreset,
) -> Option<Transform> {
    let width = dimensions.width();
    let height = dimensions.height();
    let (x, y, scale_width, scale_height) = if position == OverlayPositionPreset::FullFrame {
        (0, 0, width, height)
    } else {
        let scale_width = (width / 3).max(1);
        let scale_height = (height / 3).max(1);
        let margin = (width.min(height) / 32).max(1);
        let left = margin.min(width.saturating_sub(scale_width));
        let top = margin.min(height.saturating_sub(scale_height));
        let right = width.saturating_sub(scale_width).saturating_sub(margin);
        let bottom = height.saturating_sub(scale_height).saturating_sub(margin);
        let (x, y) = match position {
            OverlayPositionPreset::FullFrame | OverlayPositionPreset::TopLeft => (left, top),
            OverlayPositionPreset::TopRight => (right, top),
            OverlayPositionPreset::BottomLeft => (left, bottom),
            OverlayPositionPreset::BottomRight => (right, bottom),
        };
        (x, y, scale_width, scale_height)
    };
    Some(Transform::new(
        i32::try_from(x).ok()?,
        i32::try_from(y).ok()?,
        scale_width,
        scale_height,
        Rotation::Deg0,
    ))
}

/// The inset white border width, in pixels, of an overlay laid out by
/// `transform`: none, thin (`min_side / 360`, at least 1), or thick
/// (`min_side / 120`, at least 2).
#[must_use]
pub const fn overlay_border_width(border: OverlayBorderPreset, transform: Transform) -> u32 {
    let minimum_dimension = if transform.scale_width < transform.scale_height {
        transform.scale_width
    } else {
        transform.scale_height
    };
    match border {
        OverlayBorderPreset::None => 0,
        OverlayBorderPreset::ThinWhite => {
            let scaled = minimum_dimension / 360;
            if scaled < 1 { 1 } else { scaled }
        }
        OverlayBorderPreset::ThickWhite => {
            let scaled = minimum_dimension / 120;
            if scaled < 2 { 2 } else { scaled }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dimensions(width: u32, height: u32) -> VideoDimensions {
        VideoDimensions::new(width, height).unwrap()
    }

    fn transform(x: i32, y: i32, width: u32, height: u32) -> Transform {
        Transform::new(x, y, width, height, Rotation::Deg0)
    }

    #[test]
    fn native_1080p_geometry_is_pinned() {
        let hd = dimensions(1_920, 1_080);
        assert_eq!(
            overlay_transform(hd, OverlayPositionPreset::FullFrame),
            Some(transform(0, 0, 1_920, 1_080))
        );
        assert_eq!(
            overlay_transform(hd, OverlayPositionPreset::TopLeft),
            Some(transform(33, 33, 640, 360))
        );
        assert_eq!(
            overlay_transform(hd, OverlayPositionPreset::TopRight),
            Some(transform(1_247, 33, 640, 360))
        );
        assert_eq!(
            overlay_transform(hd, OverlayPositionPreset::BottomLeft),
            Some(transform(33, 687, 640, 360))
        );
        assert_eq!(
            overlay_transform(hd, OverlayPositionPreset::BottomRight),
            Some(transform(1_247, 687, 640, 360))
        );
    }

    #[test]
    fn monitor_size_geometry_is_pinned() {
        // The simulated monitor tap evaluates the same presets at 640x360.
        let monitor = dimensions(640, 360);
        assert_eq!(
            overlay_transform(monitor, OverlayPositionPreset::TopRight),
            Some(transform(416, 11, 213, 120))
        );
        assert_eq!(
            overlay_transform(monitor, OverlayPositionPreset::BottomLeft),
            Some(transform(11, 229, 213, 120))
        );
    }

    #[test]
    fn tiny_canvases_keep_one_pixel_overlays_inside_the_frame() {
        let tiny = dimensions(2, 2);
        assert_eq!(
            overlay_transform(tiny, OverlayPositionPreset::TopLeft),
            Some(transform(1, 1, 1, 1))
        );
        assert_eq!(
            overlay_transform(tiny, OverlayPositionPreset::BottomRight),
            Some(transform(0, 0, 1, 1))
        );
    }

    #[test]
    fn border_presets_scale_with_the_shorter_side() {
        let small = transform(0, 0, 640, 360);
        assert_eq!(overlay_border_width(OverlayBorderPreset::None, small), 0);
        assert_eq!(
            overlay_border_width(OverlayBorderPreset::ThinWhite, small),
            1
        );
        assert_eq!(
            overlay_border_width(OverlayBorderPreset::ThickWhite, small),
            3
        );
        let monitor = transform(0, 0, 213, 120);
        assert_eq!(
            overlay_border_width(OverlayBorderPreset::ThinWhite, monitor),
            1
        );
        assert_eq!(
            overlay_border_width(OverlayBorderPreset::ThickWhite, monitor),
            2
        );
        let uhd = transform(0, 0, 3_840, 2_160);
        assert_eq!(overlay_border_width(OverlayBorderPreset::ThinWhite, uhd), 6);
        assert_eq!(
            overlay_border_width(OverlayBorderPreset::ThickWhite, uhd),
            18
        );
    }
}

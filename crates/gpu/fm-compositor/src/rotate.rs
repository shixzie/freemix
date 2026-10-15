//! Arbitrary-angle layer rotation shared by the CPU executor and the native
//! uniform encoder.
//!
//! A tilted layer is first drawn exactly as an untilted one into an
//! offscreen buffer the size of its transform footprint (the scaled,
//! quarter-turned box, with its inset border). Every output pixel in the
//! rotated footprint's bounding box then inverse-maps its centre into that
//! buffer with the shared Q30 sine and cosine ([`fm_animation::sin_cos_q30`],
//! exact at multiples of 90 degrees), floors once to Q8, and samples the
//! premultiplied buffer bilinearly with 8-bit weights, transparent outside.
//! Positive angles turn clockwise on screen, as in `fm-titles`.

use fm_animation::{FULL_TURN_MILLIDEGREES, TRIG_ONE, sin_cos_q30};
use fm_video::{FrameError, ImageFrame, Transform};

/// The footprint `(width, height)` of `transform`: a quarter turn swaps the
/// scaled sides.
pub(crate) const fn footprint(transform: Transform) -> (u32, u32) {
    match transform.rotation {
        fm_video::Rotation::Deg0 | fm_video::Rotation::Deg180 => {
            (transform.scale_width, transform.scale_height)
        }
        fm_video::Rotation::Deg90 | fm_video::Rotation::Deg270 => {
            (transform.scale_height, transform.scale_width)
        }
    }
}

/// Where a tilted footprint lands: its size, doubled centre, the rotation,
/// and the half-open output rows and columns its rotated bounding box
/// (widened by one pixel for the bilinear taps) covers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Tilt {
    pub width: u32,
    pub height: u32,
    pub double_center_x: i64,
    pub double_center_y: i64,
    /// Q30.
    pub sin: i64,
    pub cos: i64,
    pub x0: i64,
    pub y0: i64,
    pub x1: i64,
    pub y1: i64,
}

impl Tilt {
    /// The tilt of `transform` turned by `millidegrees`, or `None` when the
    /// angle is a whole turn (the layer draws untilted).
    pub(crate) fn new(transform: Transform, millidegrees: i32) -> Option<Self> {
        let millidegrees = i64::from(millidegrees).rem_euclid(FULL_TURN_MILLIDEGREES);
        if millidegrees == 0 {
            return None;
        }
        let (width, height) = footprint(transform);
        let (sin, cos) = sin_cos_q30(millidegrees);
        let double_center_x = 2 * i64::from(transform.translation_x) + i64::from(width);
        let double_center_y = 2 * i64::from(transform.translation_y) + i64::from(height);
        let (wide, tall) = (i128::from(width), i128::from(height));
        let (sin128, cos128) = (i128::from(sin), i128::from(cos));
        // Doubled half extents of the rotated box, in Q30.
        let extent_x = cos128.abs() * wide + sin128.abs() * tall;
        let extent_y = sin128.abs() * wide + cos128.abs() * tall;
        let one = i128::from(TRIG_ONE);
        let low = |center2: i64, extent: i128| {
            i64::try_from((i128::from(center2) * one - extent).div_euclid(2 * one) - 1)
                .unwrap_or(i64::MIN)
        };
        let high = |center2: i64, extent: i128| {
            i64::try_from((i128::from(center2) * one + extent).div_euclid(2 * one) + 2)
                .unwrap_or(i64::MAX)
        };
        Some(Self {
            width,
            height,
            double_center_x,
            double_center_y,
            sin,
            cos,
            x0: low(double_center_x, extent_x),
            y0: low(double_center_y, extent_y),
            x1: high(double_center_x, extent_x),
            y1: high(double_center_y, extent_y),
        })
    }

    /// Whether any covered pixel lies on a `width` by `height` output.
    #[cfg_attr(not(feature = "native-wgpu"), allow(dead_code))]
    pub(crate) fn touches(&self, width: u32, height: u32) -> bool {
        self.x0 < i64::from(width) && self.y0 < i64::from(height) && self.x1 > 0 && self.y1 > 0
    }

    /// The footprint position, in Q8 pixel-centre coordinates, that output
    /// pixel `(x, y)` inverse-maps to: `u = cos*dx + sin*dy + (w - 1)/2` and
    /// `v = -sin*dx + cos*dy + (h - 1)/2`, where `(dx, dy)` is the pixel
    /// centre relative to the footprint centre. Floors once.
    pub(crate) fn source_q8(&self, x: i64, y: i64) -> (i64, i64) {
        let dx2 = i128::from(2 * x + 1 - self.double_center_x);
        let dy2 = i128::from(2 * y + 1 - self.double_center_y);
        let (sin, cos) = (i128::from(self.sin), i128::from(self.cos));
        let one = i128::from(TRIG_ONE);
        let u = cos * dx2 + sin * dy2 + (i128::from(self.width) - 1) * one;
        let v = -sin * dx2 + cos * dy2 + (i128::from(self.height) - 1) * one;
        (
            i64::try_from(u >> 23).unwrap_or(i64::MIN),
            i64::try_from(v >> 23).unwrap_or(i64::MIN),
        )
    }

    /// Draws the premultiplied `footprint` buffer (exactly the footprint
    /// size) turned into a transparent `output_width` x `output_height`
    /// frame.
    pub(crate) fn draw(
        &self,
        footprint: &ImageFrame,
        output_width: u32,
        output_height: u32,
    ) -> Result<ImageFrame, FrameError> {
        let stride = usize::try_from(output_width)
            .ok()
            .and_then(|width| width.checked_mul(4))
            .ok_or(FrameError::LayoutOverflow)?;
        let mut bytes = vec![
            0_u8;
            stride
                .checked_mul(
                    usize::try_from(output_height).map_err(|_| FrameError::LayoutOverflow)?
                )
                .ok_or(FrameError::LayoutOverflow)?
        ];
        let x0 = self.x0.clamp(0, i64::from(output_width));
        let x1 = self.x1.clamp(0, i64::from(output_width));
        let y0 = self.y0.clamp(0, i64::from(output_height));
        let y1 = self.y1.clamp(0, i64::from(output_height));
        let width = i64::from(self.width);
        let height = i64::from(self.height);
        let texel = |x: i64, y: i64| -> [u32; 4] {
            if x < 0 || y < 0 || x >= width || y >= height {
                return [0; 4];
            }
            let (Ok(x), Ok(y)) = (u32::try_from(x), u32::try_from(y)) else {
                return [0; 4];
            };
            footprint.pixel(x, y).map_or([0; 4], |pixel| {
                [
                    u32::from(pixel.r),
                    u32::from(pixel.g),
                    u32::from(pixel.b),
                    u32::from(pixel.a),
                ]
            })
        };
        for y in y0..y1 {
            for x in x0..x1 {
                let (source_x, source_y) = self.source_q8(x, y);
                let (column, row) = (source_x >> 8, source_y >> 8);
                if column < -1 || row < -1 || column >= width || row >= height {
                    continue;
                }
                let fx = u32::try_from(source_x & 255).unwrap_or(0);
                let fy = u32::try_from(source_y & 255).unwrap_or(0);
                let taps = [
                    texel(column, row),
                    texel(column + 1, row),
                    texel(column, row + 1),
                    texel(column + 1, row + 1),
                ];
                let mut channels = [0_u8; 4];
                for (channel, value) in channels.iter_mut().enumerate() {
                    let top = taps[0][channel] * (256 - fx) + taps[1][channel] * fx;
                    let bottom = taps[2][channel] * (256 - fx) + taps[3][channel] * fx;
                    let mixed = (top * (256 - fy) + bottom * fy + 32_768) >> 16;
                    *value = u8::try_from(mixed).unwrap_or(u8::MAX);
                }
                if channels == [0; 4] {
                    continue;
                }
                // `x` and `y` are clamped to the output, so the offset fits.
                let offset =
                    usize::try_from(y).unwrap_or(0) * stride + usize::try_from(x).unwrap_or(0) * 4;
                if let Some(pixel) = bytes.get_mut(offset..offset + 4) {
                    pixel.copy_from_slice(&channels);
                }
            }
        }
        ImageFrame::new(output_width, output_height, stride, bytes)
    }
}

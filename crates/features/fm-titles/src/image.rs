//! Integer placement and resampling of decoded images inside element bounds.
//!
//! Everything here is exact integer arithmetic so a given scene, source frame,
//! and canvas always produce the same pixels on every host. Wide intermediate
//! types (`i128`) are used because a cover-fitted image of a tall thin source
//! in a huge element can be far larger than `u32`; only the canvas
//! intersection is ever iterated, so those sizes never set a loop length.

use crate::{Alignment, Bounds, HorizontalAlignment, ImageFilter, ImageFit, VerticalAlignment};
use fm_video::{ImageFrame, Rgba8};

/// Fractional bits of a bilinear sample coordinate and weight.
const FRACTION_BITS: u32 = 8;
const FRACTION_ONE: i128 = 1 << FRACTION_BITS;

/// The placed image rectangle in canvas coordinates. It may extend beyond the
/// element bounds (cover, none) or leave part of them empty (contain).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Placement {
    pub(crate) x: i128,
    pub(crate) y: i128,
    pub(crate) width: i128,
    pub(crate) height: i128,
}

/// Places a `source_width` by `source_height` image inside `bounds`.
///
/// Returns `None` when either the bounds or the source is empty.
pub(crate) fn place(
    bounds: Bounds,
    source_width: u32,
    source_height: u32,
    fit: ImageFit,
    alignment: Alignment,
) -> Option<Placement> {
    let bound_width = i128::from(bounds.width);
    let bound_height = i128::from(bounds.height);
    let source_width = i128::from(source_width);
    let source_height = i128::from(source_height);
    if bound_width == 0 || bound_height == 0 || source_width == 0 || source_height == 0 {
        return None;
    }
    let (width, height) = match fit {
        ImageFit::Stretch => (bound_width, bound_height),
        ImageFit::None => (source_width, source_height),
        ImageFit::Contain | ImageFit::Cover => {
            // Compare the aspect ratios exactly by cross-multiplying.
            let width_limited = bound_width * source_height <= bound_height * source_width;
            // Contain scales by the limiting axis; cover by the other one, and
            // its free axis may never round below the bounds it must cover.
            let contain = fit == ImageFit::Contain;
            if width_limited == contain {
                let height = scaled(source_height, bound_width, source_width);
                (
                    bound_width,
                    if contain {
                        height
                    } else {
                        height.max(bound_height)
                    },
                )
            } else {
                let width = scaled(source_width, bound_height, source_height);
                (
                    if contain {
                        width
                    } else {
                        width.max(bound_width)
                    },
                    bound_height,
                )
            }
        }
    };
    let x = i128::from(bounds.x)
        + match alignment.horizontal {
            HorizontalAlignment::Start => 0,
            HorizontalAlignment::Center => (bound_width - width).div_euclid(2),
            HorizontalAlignment::End => bound_width - width,
        };
    let y = i128::from(bounds.y)
        + match alignment.vertical {
            VerticalAlignment::Start => 0,
            VerticalAlignment::Center => (bound_height - height).div_euclid(2),
            VerticalAlignment::End => bound_height - height,
        };
    Some(Placement {
        x,
        y,
        width,
        height,
    })
}

/// `round(value * numerator / denominator)` with halves rounding up, at least 1.
fn scaled(value: i128, numerator: i128, denominator: i128) -> i128 {
    ((value * numerator + denominator / 2) / denominator).max(1)
}

/// Samples the source pixel that destination offset `(local_x, local_y)` inside
/// a placed image of `placement` size maps to. Offsets must be within
/// `0..placement.width` and `0..placement.height`.
pub(crate) fn sample(
    frame: &ImageFrame,
    placement: Placement,
    local_x: i128,
    local_y: i128,
    filter: ImageFilter,
) -> Option<Rgba8> {
    let source_width = i128::from(frame.width());
    let source_height = i128::from(frame.height());
    match filter {
        ImageFilter::Nearest => {
            let x = local_x * source_width / placement.width;
            let y = local_y * source_height / placement.height;
            frame.pixel(
                clamp_index(x, source_width)?,
                clamp_index(y, source_height)?,
            )
        }
        ImageFilter::Bilinear => {
            let (x0, x1, fx) = bilinear_axis(local_x, source_width, placement.width)?;
            let (y0, y1, fy) = bilinear_axis(local_y, source_height, placement.height)?;
            let top_left = frame.pixel(x0, y0)?.to_bytes();
            let top_right = frame.pixel(x1, y0)?.to_bytes();
            let bottom_left = frame.pixel(x0, y1)?.to_bytes();
            let bottom_right = frame.pixel(x1, y1)?.to_bytes();
            let mut out = [0_u8; 4];
            for (index, channel) in out.iter_mut().enumerate() {
                let top = i128::from(top_left[index]) * (FRACTION_ONE - fx)
                    + i128::from(top_right[index]) * fx;
                let bottom = i128::from(bottom_left[index]) * (FRACTION_ONE - fx)
                    + i128::from(bottom_right[index]) * fx;
                let total = top * (FRACTION_ONE - fy) + bottom * fy;
                let half = 1_i128 << (2 * FRACTION_BITS - 1);
                *channel = u8::try_from((total + half) >> (2 * FRACTION_BITS)).unwrap_or(u8::MAX);
            }
            Some(Rgba8::new(out[0], out[1], out[2], out[3]))
        }
    }
}

/// Maps destination offset `local` to the two source indices bracketing its
/// pixel center and the 8-bit weight of the second one.
///
/// The source coordinate is `(local + 0.5) * source / placed - 0.5`, floored to
/// 1/256 px. Indices outside the source clamp to its edge.
fn bilinear_axis(local: i128, source: i128, placed: i128) -> Option<(u32, u32, i128)> {
    let coordinate = (2 * local + 1) * source * FRACTION_ONE / (2 * placed) - FRACTION_ONE / 2;
    let first = coordinate.div_euclid(FRACTION_ONE);
    let weight = coordinate.rem_euclid(FRACTION_ONE);
    Some((
        clamp_index(first, source)?,
        clamp_index(first + 1, source)?,
        weight,
    ))
}

fn clamp_index(index: i128, length: i128) -> Option<u32> {
    u32::try_from(index.clamp(0, length.checked_sub(1)?.max(0))).ok()
}

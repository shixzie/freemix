//! Deterministic CPU blends of one input's outgoing and incoming pictures:
//! a media list's transition between two items (IN-006).
//!
//! Both pictures are RGBA8 rasters of the same width, height, and stride,
//! given as raw row bytes so a decoded video frame blends without a copy
//! into an [`crate::ImageFrame`]. The incoming weight is in basis points
//! (`0..=10_000`); every channel, alpha included, blends with rounded
//! integer arithmetic, `(a * (10_000 - w) + b * w + 5_000) / 10_000`.
//! Row padding beyond the visible width is copied from the incoming
//! picture.

use crate::BlendError;

/// Full incoming weight, in basis points.
pub const FULL_ITEM_WEIGHT: u16 = 10_000;

/// How an outgoing item gives way to an incoming one.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ItemTransition {
    /// A cross-fade: each channel moves from outgoing to incoming.
    Mix,
    /// Through opaque black: over the first half of the weight the outgoing
    /// picture fades to black (`w' = 2w`), over the second the incoming one
    /// fades up from it (`w' = 2w - 10_000`).
    Dip,
    /// Columns `[0, floor(W * w / 10_000))` show the incoming picture and
    /// the rest the outgoing one (left to right, as
    /// [`crate::TransitionShape::Wipe`]).
    Wipe,
}

impl ItemTransition {
    /// The gains the outgoing and incoming items' sound plays at on a frame
    /// of incoming weight `weight`: a linear cross-fade for a mix and a wipe,
    /// and out then in for a dip.
    #[must_use]
    pub fn audio_gains(self, weight: u16) -> (f32, f32) {
        let weight = f32::from(weight.min(FULL_ITEM_WEIGHT)) / f32::from(FULL_ITEM_WEIGHT);
        match self {
            Self::Mix | Self::Wipe => (1.0 - weight, weight),
            Self::Dip if weight <= 0.5 => (1.0 - 2.0 * weight, 0.0),
            Self::Dip => (0.0, 2.0f32.mul_add(weight, -1.0)),
        }
    }
}

fn blend(outgoing: u8, incoming: u8, weight: u32) -> u8 {
    let full = u32::from(FULL_ITEM_WEIGHT);
    let value = u32::from(outgoing) * (full - weight) + u32::from(incoming) * weight + full / 2;
    u8::try_from(value / full).unwrap_or(u8::MAX)
}

/// Blends `outgoing` toward `incoming` with `kind` at incoming weight
/// `weight` basis points into `output`. All three are `height` rows of
/// `stride` bytes holding `width` RGBA8 pixels each. Weight zero yields
/// `outgoing` and full weight `incoming`, byte for byte, within the visible
/// width.
///
/// # Errors
///
/// Returns [`BlendError::NumeratorExceedsDenominator`] for a weight above
/// [`FULL_ITEM_WEIGHT`], [`BlendError::StrideMismatch`] when the stride
/// cannot hold a row, and [`BlendError::WidthMismatch`] when a buffer is
/// not `stride * height` bytes.
pub fn blend_items(
    kind: ItemTransition,
    outgoing: &[u8],
    incoming: &[u8],
    output: &mut [u8],
    (width, height, stride): (u32, u32, usize),
    weight: u16,
) -> Result<(), BlendError> {
    if weight > FULL_ITEM_WEIGHT {
        return Err(BlendError::NumeratorExceedsDenominator {
            numerator: u32::from(weight),
            denominator: u32::from(FULL_ITEM_WEIGHT),
        });
    }
    let row_bytes = usize::try_from(width)
        .unwrap_or(usize::MAX)
        .saturating_mul(4);
    if stride < row_bytes {
        return Err(BlendError::StrideMismatch {
            left: stride,
            right: row_bytes,
        });
    }
    let total = stride.saturating_mul(usize::try_from(height).unwrap_or(usize::MAX));
    for buffer in [outgoing.len(), incoming.len(), output.len()] {
        if buffer != total {
            return Err(BlendError::WidthMismatch {
                left: u32::try_from(buffer).unwrap_or(u32::MAX),
                right: u32::try_from(total).unwrap_or(u32::MAX),
            });
        }
    }
    output.copy_from_slice(incoming);
    let weight = u32::from(weight);
    let full = u32::from(FULL_ITEM_WEIGHT);
    let rows = outgoing
        .chunks_exact(stride)
        .zip(incoming.chunks_exact(stride))
        .zip(output.chunks_exact_mut(stride));
    match kind {
        ItemTransition::Mix => {
            for ((from, to), out) in rows {
                for ((a, b), o) in from[..row_bytes]
                    .iter()
                    .zip(&to[..row_bytes])
                    .zip(&mut out[..row_bytes])
                {
                    *o = blend(*a, *b, weight);
                }
            }
        }
        ItemTransition::Dip => {
            // Opaque black between the two halves.
            let (source_is_outgoing, toward_black) = if weight * 2 <= full {
                (true, weight * 2)
            } else {
                (false, full - (weight * 2 - full))
            };
            for ((from, to), out) in rows {
                let source = if source_is_outgoing { from } else { to };
                for (index, (value, o)) in source[..row_bytes]
                    .iter()
                    .zip(&mut out[..row_bytes])
                    .enumerate()
                {
                    // Black is opaque: alpha moves toward full.
                    let black = if index % 4 == 3 { u8::MAX } else { 0 };
                    *o = blend(*value, black, toward_black);
                }
            }
        }
        ItemTransition::Wipe => {
            let edge = crate::transition_extent(width, weight, full);
            let split = usize::try_from(edge)
                .unwrap_or(usize::MAX)
                .saturating_mul(4);
            for ((from, _), out) in rows {
                out[split..row_bytes].copy_from_slice(&from[split..row_bytes]);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZE: (u32, u32, usize) = (4, 2, 20);

    /// A 4x2 picture of one colour, with four padding bytes per row.
    fn picture(rgba: [u8; 4]) -> Vec<u8> {
        let mut pixels = Vec::new();
        for _ in 0..2 {
            pixels.extend(rgba.repeat(4));
            pixels.extend([9, 9, 9, 9]);
        }
        pixels
    }

    fn blended(kind: ItemTransition, weight: u16) -> Vec<u8> {
        let mut output = vec![0; 40];
        blend_items(
            kind,
            &picture([200, 100, 0, 255]),
            &picture([0, 50, 250, 255]),
            &mut output,
            SIZE,
            weight,
        )
        .unwrap();
        output
    }

    fn pixel(output: &[u8], x: usize, y: usize) -> [u8; 4] {
        let at = y * 20 + x * 4;
        [output[at], output[at + 1], output[at + 2], output[at + 3]]
    }

    #[test]
    fn every_blend_starts_at_the_outgoing_picture_and_ends_at_the_incoming_one() {
        for kind in [
            ItemTransition::Mix,
            ItemTransition::Dip,
            ItemTransition::Wipe,
        ] {
            assert_eq!(
                pixel(&blended(kind, 0), 1, 1),
                [200, 100, 0, 255],
                "{kind:?}"
            );
            assert_eq!(
                pixel(&blended(kind, FULL_ITEM_WEIGHT), 3, 0),
                [0, 50, 250, 255],
                "{kind:?}"
            );
        }
    }

    #[test]
    fn a_mix_rounds_each_channel() {
        let output = blended(ItemTransition::Mix, 2_500);
        assert_eq!(pixel(&output, 2, 1), [150, 88, 63, 255]);
        // Padding comes from the incoming picture.
        assert_eq!(&output[16..20], &[9, 9, 9, 9]);
    }

    #[test]
    fn a_dip_passes_through_opaque_black_at_half_weight() {
        assert_eq!(
            pixel(&blended(ItemTransition::Dip, 5_000), 0, 0),
            [0, 0, 0, 255]
        );
        // A quarter of the way the outgoing picture is half dimmed; three
        // quarters of the way the incoming one is half up.
        assert_eq!(
            pixel(&blended(ItemTransition::Dip, 2_500), 0, 0),
            [100, 50, 0, 255]
        );
        assert_eq!(
            pixel(&blended(ItemTransition::Dip, 7_500), 0, 0),
            [0, 25, 125, 255]
        );
    }

    #[test]
    fn a_wipe_shows_the_incoming_picture_left_of_its_edge() {
        let output = blended(ItemTransition::Wipe, 5_000);
        for y in 0..2 {
            assert_eq!(pixel(&output, 1, y), [0, 50, 250, 255]);
            assert_eq!(pixel(&output, 2, y), [200, 100, 0, 255]);
        }
    }

    #[test]
    fn mismatched_buffers_and_weights_are_refused() {
        let mut output = vec![0; 40];
        let small = vec![0; 39];
        let full = picture([1, 2, 3, 4]);
        assert!(blend_items(ItemTransition::Mix, &small, &full, &mut output, SIZE, 1).is_err());
        assert!(blend_items(ItemTransition::Mix, &full, &full, &mut output, SIZE, 10_001).is_err());
        assert!(
            blend_items(
                ItemTransition::Mix,
                &full,
                &full,
                &mut output,
                (6, 2, 20),
                1
            )
            .is_err()
        );
    }

    #[test]
    fn sound_follows_the_picture() {
        assert_eq!(ItemTransition::Mix.audio_gains(2_500), (0.75, 0.25));
        assert_eq!(ItemTransition::Wipe.audio_gains(10_000), (0.0, 1.0));
        assert_eq!(ItemTransition::Dip.audio_gains(2_500), (0.5, 0.0));
        assert_eq!(ItemTransition::Dip.audio_gains(7_500), (0.0, 0.5));
    }
}

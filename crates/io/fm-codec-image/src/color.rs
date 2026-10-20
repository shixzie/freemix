//! Embedded ICC profile conversion to sRGB.
//!
//! Profiles are parsed and applied with `moxcms`. Conversion is best effort:
//! anything that cannot be parsed or applied leaves the pixels untouched and
//! reports [`StillColorProfile::IgnoredUnsupported`].

use image::RgbaImage;
use moxcms::{ColorProfile, DataColorSpace, Layout, TransformOptions};

use crate::StillColorProfile;

/// Pixels converted per transform call; bounds the scratch buffers.
const CHUNK_PIXELS: usize = 16 * 1024;
/// Largest per-channel difference for a probe to count as an identity.
const IDENTITY_TOLERANCE: u8 = 1;

/// Converts `rgba` from the embedded `profile` to sRGB in place.
///
/// `source_is_gray` tells whether the decoded source was greyscale, which is
/// the only case a grey profile applies to (its channels are then equal).
pub(crate) fn convert_to_srgb(
    profile: &[u8],
    rgba: &mut RgbaImage,
    source_is_gray: bool,
) -> StillColorProfile {
    let Ok(source) = ColorProfile::new_from_slice(profile) else {
        return StillColorProfile::IgnoredUnsupported;
    };
    let srgb = ColorProfile::new_srgb();
    let pixels: &mut [u8] = rgba;
    match source.color_space {
        DataColorSpace::Rgb => convert_rgb(&source, &srgb, pixels),
        DataColorSpace::Gray if source_is_gray => convert_gray(&source, &srgb, pixels),
        _ => StillColorProfile::IgnoredUnsupported,
    }
}

fn convert_rgb(source: &ColorProfile, srgb: &ColorProfile, pixels: &mut [u8]) -> StillColorProfile {
    let Ok(transform) = source.create_transform_8bit(
        Layout::Rgba,
        srgb,
        Layout::Rgba,
        TransformOptions::default(),
    ) else {
        return StillColorProfile::IgnoredUnsupported;
    };

    // Probe a 16^3 lattice of opaque colours to detect sRGB-equivalent
    // profiles, which are left untouched to avoid rounding drift.
    let lattice: Vec<u8> = (0..16u8)
        .flat_map(|red| {
            (0..16u8).flat_map(move |green| {
                (0..16u8).flat_map(move |blue| [red * 17, green * 17, blue * 17, u8::MAX])
            })
        })
        .collect();
    let mut probed = vec![0; lattice.len()];
    if transform.transform(&lattice, &mut probed).is_err() {
        return StillColorProfile::IgnoredUnsupported;
    }
    if is_identity(&lattice, &probed) {
        return StillColorProfile::EmbeddedSrgb;
    }

    let mut scratch = vec![0; CHUNK_PIXELS.min(pixels.len() / 4) * 4];
    for chunk in pixels.chunks_mut(CHUNK_PIXELS * 4) {
        let output = &mut scratch[..chunk.len()];
        // Lengths are whole RGBA pixels and the executor already ran on the
        // probe, so a failure here is not expected; keep what converted.
        if transform.transform(chunk, output).is_err() {
            return StillColorProfile::IgnoredUnsupported;
        }
        chunk.copy_from_slice(output);
    }
    StillColorProfile::ConvertedToSrgb
}

fn convert_gray(
    source: &ColorProfile,
    srgb: &ColorProfile,
    pixels: &mut [u8],
) -> StillColorProfile {
    let Ok(transform) = source.create_transform_8bit(
        Layout::GrayAlpha,
        srgb,
        Layout::Rgba,
        TransformOptions::default(),
    ) else {
        return StillColorProfile::IgnoredUnsupported;
    };

    let ramp: Vec<u8> = (0..=u8::MAX).flat_map(|value| [value, u8::MAX]).collect();
    let mut probed = vec![0; 256 * 4];
    if transform.transform(&ramp, &mut probed).is_err() {
        return StillColorProfile::IgnoredUnsupported;
    }
    let expected: Vec<u8> = (0..=u8::MAX)
        .flat_map(|value| [value, value, value, u8::MAX])
        .collect();
    if is_identity(&expected, &probed) {
        return StillColorProfile::EmbeddedSrgb;
    }

    let chunk_pixels = CHUNK_PIXELS.min(pixels.len() / 4);
    let mut gray = vec![0; chunk_pixels * 2];
    let mut converted = vec![0; chunk_pixels * 4];
    for chunk in pixels.chunks_mut(CHUNK_PIXELS * 4) {
        let count = chunk.len() / 4;
        let gray = &mut gray[..count * 2];
        for (pair, pixel) in gray
            .as_chunks_mut::<2>()
            .0
            .iter_mut()
            .zip(chunk.as_chunks::<4>().0)
        {
            pair[0] = pixel[0];
            pair[1] = pixel[3];
        }
        let output = &mut converted[..chunk.len()];
        if transform.transform(gray, output).is_err() {
            return StillColorProfile::IgnoredUnsupported;
        }
        chunk.copy_from_slice(output);
    }
    StillColorProfile::ConvertedToSrgb
}

fn is_identity(expected: &[u8], actual: &[u8]) -> bool {
    expected
        .iter()
        .zip(actual)
        .all(|(expected, actual)| expected.abs_diff(*actual) <= IDENTITY_TOLERANCE)
}

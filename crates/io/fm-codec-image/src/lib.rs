#![forbid(unsafe_code)]

//! Bounded decoding of still images for live video "Image" and "Slideshow"
//! inputs.
//!
//! Formats are recognised from their signatures, never from file names:
//! PNG (including APNG), JPEG, WebP (lossy, lossless, animated), GIF
//! (including animated), BMP, TIFF and SVG. Animated sources decode their
//! first frame as the still (for APNG this is the default image) and report
//! [`DecodedStill::animated`].
//!
//! Every decoded still is a tightly packed straight-alpha RGBA8 frame tagged
//! as full-range sRGB BT.709 RGB:
//!
//! - EXIF orientation is applied for every format whose decoder reports it
//!   (JPEG, WebP, TIFF, PNG `eXIf`).
//! - Alpha is preserved for every format and colour type that carries it;
//!   [`DecodedStill::source_has_alpha`] reports whether the source did.
//! - An embedded ICC profile is converted to sRGB with `moxcms` (RGB
//!   profiles, and grey profiles on grey images). A profile equivalent to
//!   sRGB leaves pixels untouched; an unparsable or unsupported profile is
//!   ignored and the pixels are treated as sRGB. The outcome is reported by
//!   [`DecodedStill::color_profile`]. Oversized profiles are still a typed
//!   [`StillDecodeError::IccProfileTooLarge`] error.
//! - SVG is rasterised with `resvg` without text support (no fonts are
//!   loaded, so `<text>` is not drawn) and without any filesystem access;
//!   embedded data-URL PNG/JPEG/GIF/WebP images are drawn when their
//!   dimensions fit the decode limits. [`decode_still`] renders an SVG at its
//!   intrinsic size; [`decode_still_for_output`] renders it directly at the
//!   placement chosen for an output raster so it stays crisp.
//!
//! [`fit_still`] places a decoded frame into an output raster with one of the
//! [`OutputScaling`] policies, resampling in premultiplied alpha.

use std::fmt;
use std::io::Cursor;

mod color;
mod fit;
mod sequence;
mod svg;

pub use fit::fit_still;
pub use sequence::{
    AlphaCoverage, ImageSequenceConfig, ImageSequenceSource, ImageSequenceSourceError,
    SequenceSample, alpha_coverage,
};

use fm_frame::{
    AlphaMode, ChromaLocation, ColorMetadata, ColorPrimaries, CpuVideoFrame, CpuVideoPayload,
    CpuVideoPlane, MatrixCoefficients, MediaTiming, PixelFormat, SignalRange, TransferFunction,
    VideoDimensions, VideoFrameMetadata, VideoFrameMetadataError, VideoPayloadError,
};
use fm_types::OutputScaling;
use image::metadata::Orientation;
use image::{ColorType, DynamicImage, ImageDecoder, ImageError, ImageFormat, ImageReader, Limits};

const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
const JPEG_SIGNATURE: &[u8; 2] = b"\xff\xd8";
const ICC_JPEG_SIGNATURE: &[u8; 12] = b"ICC_PROFILE\0";

/// Number of leading bytes [`sniff_still_format`] needs to recognise every
/// supported format. Binary formats need at most 12 bytes; the rest leaves
/// room for an SVG's XML declaration, doctype and leading comments before
/// the `<svg` root element.
pub const STILL_SNIFF_BYTES: usize = 4096;

/// Lowercase file extensions of the still-image formats this crate decodes.
pub const STILL_FILE_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "webp", "gif", "bmp", "tif", "tiff", "svg",
];

/// True when `name` ends in one of [`STILL_FILE_EXTENSIONS`], compared
/// case-insensitively. A name that is only an extension (`.png`) does not
/// match. Decoding still sniffs the content; this is only a listing filter.
#[must_use]
pub fn is_still_file_name(name: &str) -> bool {
    let Some((stem, extension)) = name.rsplit_once('.') else {
        return false;
    };
    !stem.is_empty()
        && STILL_FILE_EXTENSIONS
            .iter()
            .any(|supported| extension.eq_ignore_ascii_case(supported))
}

/// Encoded still-image formats accepted by [`decode_still`].
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum StillFormat {
    Png,
    Jpeg,
    Webp,
    Gif,
    Bmp,
    Tiff,
    Svg,
}

/// What happened to an embedded ICC colour profile.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum StillColorProfile {
    /// The source carried no ICC profile; pixels are treated as sRGB.
    #[default]
    None,
    /// The embedded profile is equivalent to sRGB (every probed value maps
    /// to itself within one code value), so pixels were left untouched.
    EmbeddedSrgb,
    /// Pixels were converted from the embedded profile to sRGB.
    ConvertedToSrgb,
    /// The embedded profile could not be parsed or is not an RGB (or, for
    /// grey images, grey) profile; pixels are treated as sRGB.
    IgnoredUnsupported,
}

/// Caller-controlled resource bounds for one still-image decode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StillDecodeLimits {
    pub max_encoded_bytes: usize,
    pub max_width: u32,
    pub max_height: u32,
    pub max_decoded_rgba_bytes: usize,
    pub max_icc_bytes: usize,
    /// Budget passed to `image` for its decoder allocations.
    pub max_image_alloc_bytes: u64,
}

impl Default for StillDecodeLimits {
    fn default() -> Self {
        Self {
            max_encoded_bytes: 64 * 1024 * 1024,
            max_width: CpuVideoPayload::MAX_WIDTH,
            max_height: CpuVideoPayload::MAX_HEIGHT,
            max_decoded_rgba_bytes: CpuVideoPayload::MAX_TOTAL_BYTES,
            max_icc_bytes: 4 * 1024 * 1024,
            max_image_alloc_bytes: 512 * 1024 * 1024,
        }
    }
}

/// A decoded, tightly packed, straight-alpha RGBA8 still frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodedStill {
    pub frame: CpuVideoFrame,
    pub format: StillFormat,
    /// Dimensions encoded in the source, before EXIF orientation. For SVG
    /// this is the intrinsic size rounded up to whole pixels.
    pub source_dimensions: VideoDimensions,
    pub orientation_applied: bool,
    /// True when the source format and colour type carry alpha (always for
    /// SVG; for GIF, when the first frame has a transparent pixel).
    pub source_has_alpha: bool,
    /// True when the source is an animation (APNG, animated GIF or WebP);
    /// only its first frame was decoded.
    pub animated: bool,
    /// Outcome of embedded ICC profile handling.
    pub color_profile: StillColorProfile,
}

/// Failure from bounded still-image decoding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StillDecodeError {
    EmptyInput,
    EncodedBytesTooLarge {
        actual: usize,
        maximum: usize,
    },
    UnsupportedFormat,
    /// No longer produced: APNG input decodes its default image.
    AnimatedPngUnsupported,
    CorruptInput {
        format: StillFormat,
    },
    WidthTooLarge {
        actual: u32,
        maximum: u32,
    },
    HeightTooLarge {
        actual: u32,
        maximum: u32,
    },
    DecodedRgbaTooLarge {
        required: u64,
        maximum: usize,
    },
    IccProfileTooLarge {
        required: usize,
        maximum: usize,
    },
    /// No longer produced: embedded ICC profiles are converted or ignored.
    EmbeddedIccUnsupported,
    ImageAllocationLimitExceeded {
        required: Option<u64>,
        maximum: u64,
    },
    /// [`fit_still`] accepts only RGBA8 frames.
    FitUnsupportedPixelFormat {
        format: PixelFormat,
    },
    /// The requested output raster exceeds the CPU frame payload limits.
    FitOutputTooLarge {
        width: u32,
        height: u32,
    },
    /// The placement rectangle for the requested scaling does not fit `u32`.
    FitViewportOverflow,
    FramePayload(VideoPayloadError),
    FrameMetadata(VideoFrameMetadataError),
}

impl fmt::Display for StillDecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyInput => formatter.write_str("still image input is empty"),
            Self::EncodedBytesTooLarge { actual, maximum } => write!(
                formatter,
                "encoded image is {actual} bytes, exceeding the {maximum}-byte limit"
            ),
            Self::UnsupportedFormat => formatter.write_str(
                "input is not a supported PNG, JPEG, WebP, GIF, BMP, TIFF or SVG image",
            ),
            Self::AnimatedPngUnsupported => formatter.write_str("animated PNG is not supported"),
            Self::CorruptInput { format } => write!(formatter, "corrupt {format} image"),
            Self::WidthTooLarge { actual, maximum } => {
                write!(formatter, "image width {actual} exceeds {maximum}")
            }
            Self::HeightTooLarge { actual, maximum } => {
                write!(formatter, "image height {actual} exceeds {maximum}")
            }
            Self::DecodedRgbaTooLarge { required, maximum } => write!(
                formatter,
                "decoded RGBA image requires {required} bytes, exceeding {maximum}"
            ),
            Self::IccProfileTooLarge { required, maximum } => write!(
                formatter,
                "embedded ICC profile is {required} bytes, exceeding {maximum}"
            ),
            Self::EmbeddedIccUnsupported => formatter.write_str(
                "embedded ICC profiles are unsupported; only enumerated frame metadata is supported",
            ),
            Self::ImageAllocationLimitExceeded { required, maximum } => {
                if let Some(required) = required {
                    write!(
                        formatter,
                        "image decoder requires {required} bytes, exceeding its {maximum}-byte allocation limit"
                    )
                } else {
                    write!(
                        formatter,
                        "image decoder exceeded its {maximum}-byte allocation limit"
                    )
                }
            }
            Self::FitUnsupportedPixelFormat { format } => {
                write!(formatter, "cannot fit a {format:?} frame; RGBA8 is required")
            }
            Self::FitOutputTooLarge { width, height } => write!(
                formatter,
                "output raster {width}x{height} exceeds the CPU frame limits"
            ),
            Self::FitViewportOverflow => {
                formatter.write_str("the scaled placement rectangle overflows")
            }
            Self::FramePayload(error) => {
                write!(formatter, "invalid decoded frame payload: {error}")
            }
            Self::FrameMetadata(error) => {
                write!(formatter, "invalid decoded frame metadata: {error}")
            }
        }
    }
}

impl std::error::Error for StillDecodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::FramePayload(error) => Some(error),
            Self::FrameMetadata(error) => Some(error),
            _ => None,
        }
    }
}

impl fmt::Display for StillFormat {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Png => "PNG",
            Self::Jpeg => "JPEG",
            Self::Webp => "WebP",
            Self::Gif => "GIF",
            Self::Bmp => "BMP",
            Self::Tiff => "TIFF",
            Self::Svg => "SVG",
        })
    }
}

/// Decodes still-image bytes into straight-alpha sRGB RGBA8.
///
/// Format detection uses signatures rather than file names. Animated input
/// decodes its first frame. SVG renders at its intrinsic size (see
/// [`decode_still_for_output`] to render it at an output size instead).
///
/// # Errors
///
/// Returns a typed format, corruption, metadata, resource-limit, or frame-layout
/// error.
pub fn decode_still(
    encoded: &[u8],
    timing: MediaTiming,
    limits: StillDecodeLimits,
) -> Result<DecodedStill, StillDecodeError> {
    let format = check_input(encoded, limits)?;
    if format == StillFormat::Svg {
        return svg::decode_svg(encoded, timing, limits, None);
    }

    let animated = match format {
        StillFormat::Png => inspect_png(encoded)?,
        StillFormat::Jpeg => {
            inspect_jpeg_icc(encoded, limits.max_icc_bytes)?;
            false
        }
        StillFormat::Gif => gif_has_multiple_frames(encoded),
        StillFormat::Webp => webp_is_animated(encoded),
        StillFormat::Bmp | StillFormat::Tiff | StillFormat::Svg => false,
    };

    let image_format = match format {
        StillFormat::Png => ImageFormat::Png,
        StillFormat::Jpeg => ImageFormat::Jpeg,
        StillFormat::Webp => ImageFormat::WebP,
        StillFormat::Gif => ImageFormat::Gif,
        StillFormat::Bmp => ImageFormat::Bmp,
        StillFormat::Tiff | StillFormat::Svg => ImageFormat::Tiff,
    };
    let mut reader = ImageReader::with_format(Cursor::new(encoded), image_format);
    let mut image_limits = Limits::default();
    image_limits.max_image_width = None;
    image_limits.max_image_height = None;
    image_limits.max_alloc = Some(limits.max_image_alloc_bytes);
    reader.limits(image_limits);
    let decoder = reader
        .into_decoder()
        .map_err(|error| map_image_error(&error, format, limits.max_image_alloc_bytes, None))?;

    decode_with_decoder(decoder, timing, limits, format, animated)
}

/// Decodes still-image bytes for an `output` raster placed with `scaling`.
///
/// Raster formats decode as [`decode_still`] does and are then placed with
/// [`fit_still`]. SVG is rasterised directly into the output at the
/// destination rectangle [`OutputScaling::viewport`] computes from its
/// intrinsic size, so vector edges stay crisp at any output size. The
/// returned frame is always exactly `output` sized with fully transparent
/// pixels outside the picture.
///
/// # Errors
///
/// Returns every [`decode_still`] error plus the [`fit_still`] output-size
/// and viewport errors.
pub fn decode_still_for_output(
    encoded: &[u8],
    timing: MediaTiming,
    limits: StillDecodeLimits,
    output: VideoDimensions,
    scaling: OutputScaling,
) -> Result<DecodedStill, StillDecodeError> {
    let format = check_input(encoded, limits)?;
    if format == StillFormat::Svg {
        return svg::decode_svg(encoded, timing, limits, Some((output, scaling)));
    }
    let decoded = decode_still(encoded, timing, limits)?;
    if decoded.frame.payload().dimensions() == output {
        // Every scaling policy maps an equal-size picture onto the whole
        // output one to one.
        return Ok(decoded);
    }
    let frame = fit_still(&decoded.frame, output, scaling)?;
    Ok(DecodedStill { frame, ..decoded })
}

fn check_input(encoded: &[u8], limits: StillDecodeLimits) -> Result<StillFormat, StillDecodeError> {
    if encoded.is_empty() {
        return Err(StillDecodeError::EmptyInput);
    }
    if encoded.len() > limits.max_encoded_bytes {
        return Err(StillDecodeError::EncodedBytesTooLarge {
            actual: encoded.len(),
            maximum: limits.max_encoded_bytes,
        });
    }
    sniff_still_format(encoded)
}

fn decode_with_decoder(
    mut decoder: impl ImageDecoder,
    timing: MediaTiming,
    limits: StillDecodeLimits,
    format: StillFormat,
    animated: bool,
) -> Result<DecodedStill, StillDecodeError> {
    let (width, height) = decoder.dimensions();
    let layout = validate_layout(width, height, limits)?;

    let native_bytes = decoder.total_bytes();
    if native_bytes > limits.max_image_alloc_bytes {
        return Err(StillDecodeError::ImageAllocationLimitExceeded {
            required: Some(native_bytes),
            maximum: limits.max_image_alloc_bytes,
        });
    }

    let icc_profile = decoder
        .icc_profile()
        .map_err(|error| map_image_error(&error, format, limits.max_image_alloc_bytes, None))?
        .filter(|profile| !profile.is_empty());
    if let Some(profile) = &icc_profile
        && profile.len() > limits.max_icc_bytes
    {
        return Err(StillDecodeError::IccProfileTooLarge {
            required: profile.len(),
            maximum: limits.max_icc_bytes,
        });
    }

    let orientation = decoder
        .orientation()
        .map_err(|error| map_image_error(&error, format, limits.max_image_alloc_bytes, None))?;
    let color_type = decoder.color_type();
    let source_dimensions =
        VideoDimensions::new(width, height).ok_or(StillDecodeError::CorruptInput { format })?;
    check_oriented_size(orientation, width, height, layout)?;

    let mut decoder_limits = Limits::default();
    decoder_limits.max_image_width = Some(layout.max_width);
    decoder_limits.max_image_height = Some(layout.max_height);
    decoder_limits.max_alloc = Some(limits.max_image_alloc_bytes - native_bytes);
    decoder.set_limits(decoder_limits).map_err(|error| {
        map_image_error(
            &error,
            format,
            limits.max_image_alloc_bytes,
            Some(native_bytes),
        )
    })?;
    let mut image = DynamicImage::from_decoder(decoder).map_err(|error| {
        map_image_error(
            &error,
            format,
            limits.max_image_alloc_bytes,
            Some(native_bytes),
        )
    })?;
    image.apply_orientation(orientation);
    let mut rgba = image.into_rgba8();

    let color_profile = match &icc_profile {
        None => StillColorProfile::None,
        Some(profile) => color::convert_to_srgb(
            profile,
            &mut rgba,
            matches!(
                color_type,
                ColorType::L8 | ColorType::La8 | ColorType::L16 | ColorType::La16
            ),
        ),
    };
    let source_has_alpha = if format == StillFormat::Gif {
        // GIF always decodes as RGBA; only a used transparent index matters.
        rgba.pixels().any(|pixel| pixel.0[3] != u8::MAX)
    } else {
        color_type.has_alpha()
    };

    let output_dimensions = VideoDimensions::new(rgba.width(), rgba.height())
        .ok_or(StillDecodeError::CorruptInput { format })?;
    if usize::try_from(rgba.width())
        .ok()
        .and_then(|width| width.checked_mul(4))
        .is_none()
    {
        return Err(StillDecodeError::DecodedRgbaTooLarge {
            required: layout.rgba_bytes,
            maximum: layout.max_rgba_bytes,
        });
    }
    let frame = rgba_frame(output_dimensions, rgba.into_raw(), timing)?;

    Ok(DecodedStill {
        frame,
        format,
        source_dimensions,
        orientation_applied: orientation != Orientation::NoTransforms,
        source_has_alpha,
        animated,
        color_profile,
    })
}

fn check_oriented_size(
    orientation: Orientation,
    width: u32,
    height: u32,
    layout: ValidatedLayout,
) -> Result<(), StillDecodeError> {
    let (output_width, output_height) = if matches!(
        orientation,
        Orientation::Rotate90
            | Orientation::Rotate270
            | Orientation::Rotate90FlipH
            | Orientation::Rotate270FlipH
    ) {
        (height, width)
    } else {
        (width, height)
    };
    if output_width > layout.max_width {
        return Err(StillDecodeError::WidthTooLarge {
            actual: output_width,
            maximum: layout.max_width,
        });
    }
    if output_height > layout.max_height {
        return Err(StillDecodeError::HeightTooLarge {
            actual: output_height,
            maximum: layout.max_height,
        });
    }
    Ok(())
}

/// Wraps tightly packed straight-alpha RGBA8 bytes as a still frame.
fn rgba_frame(
    dimensions: VideoDimensions,
    bytes: Vec<u8>,
    timing: MediaTiming,
) -> Result<CpuVideoFrame, StillDecodeError> {
    // The width was validated against `CpuVideoPayload::MAX_WIDTH`, so the
    // stride fits easily.
    let stride = dimensions.width() as usize * 4;
    let plane = CpuVideoPlane::new(stride, bytes).map_err(StillDecodeError::FramePayload)?;
    let payload = CpuVideoPayload::new(PixelFormat::Rgba8, dimensions, vec![plane])
        .map_err(StillDecodeError::FramePayload)?;
    CpuVideoFrame::new(timing, payload)
        .with_metadata(still_frame_metadata())
        .map_err(StillDecodeError::FrameMetadata)
}

const fn still_frame_metadata() -> VideoFrameMetadata {
    VideoFrameMetadata::new(
        ColorMetadata {
            primaries: ColorPrimaries::Bt709,
            transfer: TransferFunction::Srgb,
            matrix: MatrixCoefficients::Identity,
            range: SignalRange::Full,
            chroma_location: ChromaLocation::Center,
        },
        Some(AlphaMode::Straight),
    )
}

#[derive(Clone, Copy)]
struct ValidatedLayout {
    max_width: u32,
    max_height: u32,
    max_rgba_bytes: usize,
    rgba_bytes: u64,
}

fn validate_layout(
    width: u32,
    height: u32,
    limits: StillDecodeLimits,
) -> Result<ValidatedLayout, StillDecodeError> {
    let max_width = limits.max_width.min(CpuVideoPayload::MAX_WIDTH);
    let max_height = limits.max_height.min(CpuVideoPayload::MAX_HEIGHT);
    let max_rgba_bytes = limits
        .max_decoded_rgba_bytes
        .min(CpuVideoPayload::MAX_TOTAL_BYTES);
    if width > max_width {
        return Err(StillDecodeError::WidthTooLarge {
            actual: width,
            maximum: max_width,
        });
    }
    if height > max_height {
        return Err(StillDecodeError::HeightTooLarge {
            actual: height,
            maximum: max_height,
        });
    }

    let rgba_bytes = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or(StillDecodeError::DecodedRgbaTooLarge {
            required: u64::MAX,
            maximum: max_rgba_bytes,
        })?;
    if rgba_bytes > u64::try_from(max_rgba_bytes).unwrap_or(u64::MAX) {
        return Err(StillDecodeError::DecodedRgbaTooLarge {
            required: rgba_bytes,
            maximum: max_rgba_bytes,
        });
    }

    Ok(ValidatedLayout {
        max_width,
        max_height,
        max_rgba_bytes,
        rgba_bytes,
    })
}

/// Classifies an encoded prefix from its signature.
///
/// Binary formats are recognised from their magic bytes: PNG, JPEG
/// (`FF D8`), WebP (`RIFF????WEBP`), GIF (`GIF87a`/`GIF89a`), BMP (`BM`) and
/// TIFF (`II*\0`/`MM\0*`). SVG is recognised when, after an optional UTF-8
/// BOM, whitespace, XML declaration, processing instructions, comments and
/// doctype, the first element starts with `<svg`. Pass at least
/// [`STILL_SNIFF_BYTES`] bytes (or the whole file) so SVG preambles fit. No
/// decoding or validation occurs and file names are not considered.
///
/// # Errors
///
/// Returns [`StillDecodeError::UnsupportedFormat`] when the prefix does not
/// contain a complete supported signature.
pub fn sniff_still_format(encoded_prefix: &[u8]) -> Result<StillFormat, StillDecodeError> {
    let prefix = encoded_prefix;
    if prefix.starts_with(PNG_SIGNATURE) {
        Ok(StillFormat::Png)
    } else if prefix.starts_with(JPEG_SIGNATURE) {
        Ok(StillFormat::Jpeg)
    } else if prefix.len() >= 12 && prefix.starts_with(b"RIFF") && &prefix[8..12] == b"WEBP" {
        Ok(StillFormat::Webp)
    } else if prefix.starts_with(b"GIF87a") || prefix.starts_with(b"GIF89a") {
        Ok(StillFormat::Gif)
    } else if prefix.starts_with(b"BM") {
        Ok(StillFormat::Bmp)
    } else if prefix.starts_with(b"II*\0") || prefix.starts_with(b"MM\0*") {
        Ok(StillFormat::Tiff)
    } else if looks_like_svg(prefix) {
        Ok(StillFormat::Svg)
    } else {
        Err(StillDecodeError::UnsupportedFormat)
    }
}

fn looks_like_svg(prefix: &[u8]) -> bool {
    let mut rest = prefix.strip_prefix(b"\xef\xbb\xbf").unwrap_or(prefix);
    loop {
        rest = rest.trim_ascii_start();
        let skipped = if let Some(after) = rest.strip_prefix(b"<?") {
            skip_past(after, b"?>")
        } else if let Some(after) = rest.strip_prefix(b"<!--") {
            skip_past(after, b"-->")
        } else if rest.len() >= 9 && rest[..9].eq_ignore_ascii_case(b"<!DOCTYPE") {
            skip_doctype(&rest[9..])
        } else {
            return rest.starts_with(b"<svg")
                && rest
                    .get(4)
                    .is_some_and(|byte| byte.is_ascii_whitespace() || matches!(byte, b'>' | b'/'));
        };
        match skipped {
            Some(after) => rest = after,
            None => return false,
        }
    }
}

fn skip_past<'a>(bytes: &'a [u8], terminator: &[u8]) -> Option<&'a [u8]> {
    let position = bytes
        .windows(terminator.len())
        .position(|window| window == terminator)?;
    Some(&bytes[position + terminator.len()..])
}

fn skip_doctype(bytes: &[u8]) -> Option<&[u8]> {
    let close = bytes.iter().position(|byte| *byte == b'>')?;
    match bytes.iter().position(|byte| *byte == b'[') {
        Some(open) if open < close => skip_past(skip_past(&bytes[open..], b"]")?, b">"),
        _ => Some(&bytes[close + 1..]),
    }
}

/// True when a GIF has a second image descriptor. Malformed input reports
/// `false`; the decoder rejects it separately.
fn gif_has_multiple_frames(encoded: &[u8]) -> bool {
    fn color_table_bytes(packed: u8) -> usize {
        if packed & 0x80 == 0 {
            0
        } else {
            3 << ((packed & 0x07) + 1)
        }
    }
    fn skip_sub_blocks(encoded: &[u8], offset: &mut usize) -> Option<()> {
        loop {
            let length = usize::from(*encoded.get(*offset)?);
            *offset = offset.checked_add(1 + length)?;
            if length == 0 {
                return Some(());
            }
        }
    }
    fn count(encoded: &[u8]) -> Option<bool> {
        let mut offset = 13 + color_table_bytes(*encoded.get(10)?);
        let mut frames = 0u32;
        loop {
            match *encoded.get(offset)? {
                0x21 => {
                    offset += 2;
                    skip_sub_blocks(encoded, &mut offset)?;
                }
                0x2c => {
                    frames += 1;
                    if frames > 1 {
                        return Some(true);
                    }
                    let packed = *encoded.get(offset + 9)?;
                    offset += 10 + color_table_bytes(packed) + 1;
                    skip_sub_blocks(encoded, &mut offset)?;
                }
                _ => return Some(false),
            }
        }
    }
    count(encoded).unwrap_or(false)
}

/// True when an extended (`VP8X`) WebP sets its animation flag.
fn webp_is_animated(encoded: &[u8]) -> bool {
    encoded.get(12..16) == Some(b"VP8X") && encoded.get(20).is_some_and(|flags| flags & 0x02 != 0)
}

/// Validates PNG chunk framing up to `IEND` and reports whether an `acTL`
/// (APNG animation control) chunk is present.
fn inspect_png(encoded: &[u8]) -> Result<bool, StillDecodeError> {
    let mut offset = PNG_SIGNATURE.len();
    let mut animated = false;
    while offset < encoded.len() {
        let header_end = offset
            .checked_add(8)
            .ok_or(StillDecodeError::CorruptInput {
                format: StillFormat::Png,
            })?;
        let header = encoded
            .get(offset..header_end)
            .ok_or(StillDecodeError::CorruptInput {
                format: StillFormat::Png,
            })?;
        let length = usize::try_from(u32::from_be_bytes(header[..4].try_into().map_err(
            |_| StillDecodeError::CorruptInput {
                format: StillFormat::Png,
            },
        )?))
        .map_err(|_| StillDecodeError::CorruptInput {
            format: StillFormat::Png,
        })?;
        if &header[4..] == b"acTL" {
            animated = true;
        }
        let chunk_end = header_end
            .checked_add(length)
            .and_then(|end| end.checked_add(4))
            .ok_or(StillDecodeError::CorruptInput {
                format: StillFormat::Png,
            })?;
        encoded
            .get(header_end..chunk_end)
            .ok_or(StillDecodeError::CorruptInput {
                format: StillFormat::Png,
            })?;
        offset = chunk_end;
        if &header[4..] == b"IEND" {
            return Ok(animated);
        }
    }
    Err(StillDecodeError::CorruptInput {
        format: StillFormat::Png,
    })
}

/// Validates JPEG marker framing up to the first scan and bounds the total
/// size of `APP2` ICC profile segments before any decoding.
fn inspect_jpeg_icc(encoded: &[u8], maximum: usize) -> Result<(), StillDecodeError> {
    let mut offset = JPEG_SIGNATURE.len();
    let mut icc_bytes = 0usize;
    while offset < encoded.len() {
        while encoded.get(offset) == Some(&0xff) {
            offset += 1;
        }
        let marker = *encoded.get(offset).ok_or(StillDecodeError::CorruptInput {
            format: StillFormat::Jpeg,
        })?;
        offset += 1;
        if marker == 0xda || marker == 0xd9 {
            return Ok(());
        }
        if matches!(marker, 0x01 | 0xd0..=0xd8) {
            continue;
        }
        let length_end = offset
            .checked_add(2)
            .ok_or(StillDecodeError::CorruptInput {
                format: StillFormat::Jpeg,
            })?;
        let length_bytes =
            encoded
                .get(offset..length_end)
                .ok_or(StillDecodeError::CorruptInput {
                    format: StillFormat::Jpeg,
                })?;
        let length = usize::from(u16::from_be_bytes(length_bytes.try_into().map_err(
            |_| StillDecodeError::CorruptInput {
                format: StillFormat::Jpeg,
            },
        )?));
        if length < 2 {
            return Err(StillDecodeError::CorruptInput {
                format: StillFormat::Jpeg,
            });
        }
        let segment_end = offset
            .checked_add(length)
            .ok_or(StillDecodeError::CorruptInput {
                format: StillFormat::Jpeg,
            })?;
        let payload =
            encoded
                .get(length_end..segment_end)
                .ok_or(StillDecodeError::CorruptInput {
                    format: StillFormat::Jpeg,
                })?;
        if marker == 0xe2 && payload.starts_with(ICC_JPEG_SIGNATURE) {
            let profile_part = payload.get(ICC_JPEG_SIGNATURE.len() + 2..).ok_or(
                StillDecodeError::CorruptInput {
                    format: StillFormat::Jpeg,
                },
            )?;
            icc_bytes = icc_bytes.checked_add(profile_part.len()).ok_or(
                StillDecodeError::IccProfileTooLarge {
                    required: usize::MAX,
                    maximum,
                },
            )?;
            if icc_bytes > maximum {
                return Err(StillDecodeError::IccProfileTooLarge {
                    required: icc_bytes,
                    maximum,
                });
            }
        }
        offset = segment_end;
    }
    Err(StillDecodeError::CorruptInput {
        format: StillFormat::Jpeg,
    })
}

fn map_image_error(
    error: &ImageError,
    format: StillFormat,
    maximum: u64,
    required: Option<u64>,
) -> StillDecodeError {
    if matches!(error, ImageError::Limits(_)) {
        StillDecodeError::ImageAllocationLimitExceeded { required, maximum }
    } else {
        StillDecodeError::CorruptInput { format }
    }
}

//! SVG rasterisation with `resvg`.
//!
//! Parsing never touches the filesystem: string `href`s are refused and only
//! data-URL PNG, JPEG, GIF and WebP images whose dimensions fit the decode
//! limits are kept. Text is not rendered because no fonts are loaded.

use std::io::Cursor;
use std::sync::Arc;

use fm_frame::{MediaTiming, VideoDimensions};
use fm_types::{OutputScaling, ScaledViewport};
use image::{ImageFormat, ImageReader};
use resvg::{tiny_skia, usvg};

use crate::{
    DecodedStill, StillColorProfile, StillDecodeError, StillDecodeLimits, StillFormat, rgba_frame,
    sniff_still_format, validate_layout,
};

const CORRUPT: StillDecodeError = StillDecodeError::CorruptInput {
    format: StillFormat::Svg,
};

/// Renders an SVG at its intrinsic size, or into `output` with `scaling`.
pub(crate) fn decode_svg(
    encoded: &[u8],
    timing: MediaTiming,
    limits: StillDecodeLimits,
    output: Option<(VideoDimensions, OutputScaling)>,
) -> Result<DecodedStill, StillDecodeError> {
    let tree = usvg::Tree::from_data(encoded, &options(limits)).map_err(|_| CORRUPT)?;
    let size = tree.size();
    let source_dimensions = VideoDimensions::new(
        intrinsic_pixels(size.width()),
        intrinsic_pixels(size.height()),
    )
    .ok_or(CORRUPT)?;
    let (target, viewport) = match output {
        Some((output, scaling)) => (
            output,
            scaling
                .viewport(
                    source_dimensions.width(),
                    source_dimensions.height(),
                    output.width(),
                    output.height(),
                )
                .ok_or(StillDecodeError::FitViewportOverflow)?,
        ),
        None => (
            source_dimensions,
            ScaledViewport {
                x: 0,
                y: 0,
                width: source_dimensions.width(),
                height: source_dimensions.height(),
            },
        ),
    };
    let layout = validate_layout(target.width(), target.height(), limits)?;
    let mut pixmap = tiny_skia::Pixmap::new(target.width(), target.height()).ok_or(
        StillDecodeError::DecodedRgbaTooLarge {
            required: layout.rgba_bytes,
            maximum: layout.max_rgba_bytes,
        },
    )?;
    resvg::render(&tree, placement(viewport, size), &mut pixmap.as_mut());
    let mut rgba = pixmap.take();
    demultiply(&mut rgba);

    Ok(DecodedStill {
        frame: rgba_frame(target, rgba, timing)?,
        format: StillFormat::Svg,
        source_dimensions,
        orientation_applied: false,
        source_has_alpha: true,
        animated: false,
        color_profile: StillColorProfile::None,
    })
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the float-to-int cast saturates; huge intrinsic sizes only matter for aspect"
)]
fn intrinsic_pixels(length: f32) -> u32 {
    (length.ceil() as u32).max(1)
}

#[allow(
    clippy::cast_precision_loss,
    reason = "viewport extents and offsets are bounded raster coordinates"
)]
fn placement(viewport: ScaledViewport, size: usvg::Size) -> tiny_skia::Transform {
    tiny_skia::Transform::from_row(
        viewport.width as f32 / size.width(),
        0.0,
        0.0,
        viewport.height as f32 / size.height(),
        viewport.x as f32,
        viewport.y as f32,
    )
}

/// Converts premultiplied RGBA8 to straight alpha in place.
fn demultiply(rgba: &mut [u8]) {
    for pixel in rgba.as_chunks_mut::<4>().0 {
        let alpha = u16::from(pixel[3]);
        if alpha == 0 {
            pixel.fill(0);
        } else if alpha < 255 {
            for channel in &mut pixel[..3] {
                let straight = (u16::from(*channel) * 255 + alpha / 2) / alpha;
                *channel = u8::try_from(straight).unwrap_or(u8::MAX);
            }
        }
    }
}

fn options(limits: StillDecodeLimits) -> usvg::Options<'static> {
    usvg::Options {
        image_href_resolver: usvg::ImageHrefResolver {
            resolve_data: Box::new(move |_mime, data, _options| embedded_raster(data, limits)),
            resolve_string: Box::new(|_href, _options| None),
        },
        ..usvg::Options::default()
    }
}

type ImageKindFn = fn(Arc<Vec<u8>>) -> usvg::ImageKind;

fn embedded_raster(data: Arc<Vec<u8>>, limits: StillDecodeLimits) -> Option<usvg::ImageKind> {
    let (image_format, kind): (ImageFormat, ImageKindFn) = match sniff_still_format(&data).ok()? {
        StillFormat::Png => (ImageFormat::Png, usvg::ImageKind::PNG),
        StillFormat::Jpeg => (ImageFormat::Jpeg, usvg::ImageKind::JPEG),
        StillFormat::Gif => (ImageFormat::Gif, usvg::ImageKind::GIF),
        StillFormat::Webp => (ImageFormat::WebP, usvg::ImageKind::WEBP),
        StillFormat::Bmp | StillFormat::Tiff | StillFormat::Svg => return None,
    };
    let (width, height) = ImageReader::with_format(Cursor::new(data.as_slice()), image_format)
        .into_dimensions()
        .ok()?;
    validate_layout(width, height, limits).ok()?;
    Some(kind(data))
}

//! Per-format decoding, alpha, orientation, ICC conversion, animation and
//! SVG rasterisation. Every fixture is generated in the test.

#![allow(
    clippy::similar_names,
    clippy::cast_possible_truncation,
    reason = "fixture builders pair encoded/encoder buffers and use tiny sizes"
)]

use std::num::NonZeroU128;

use fm_codec_image::{
    DecodedStill, STILL_FILE_EXTENSIONS, STILL_SNIFF_BYTES, StillColorProfile, StillDecodeError,
    StillDecodeLimits, StillFormat, decode_still, decode_still_for_output, is_still_file_name,
    sniff_still_format,
};
use fm_frame::{
    ClockDomainId, MediaTimestamp, MediaTiming, NormalizedDuration, NormalizedTimestamp,
    OriginalTimestamp, SequenceNumber, TimeBase, VideoDimensions,
};
use fm_types::OutputScaling;
use image::codecs::bmp::BmpEncoder;
use image::codecs::gif::GifEncoder;
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::codecs::tiff::TiffEncoder;
use image::codecs::webp::WebPEncoder;
use image::{Delay, ExtendedColorType, Frame, ImageEncoder, RgbaImage};
use moxcms::{ColorProfile, Layout, TransformOptions};

fn timing() -> MediaTiming {
    let time_base = TimeBase::new(1, 90_000).unwrap();
    MediaTiming::new(
        OriginalTimestamp::new(MediaTimestamp::new(0), time_base),
        NormalizedTimestamp::from_nanos(0),
        NormalizedDuration::from_nanos(40_000_000).unwrap(),
        ClockDomainId::new(NonZeroU128::new(3).unwrap()),
        SequenceNumber::new(1),
    )
    .unwrap()
}

fn decode(encoded: &[u8]) -> DecodedStill {
    decode_still(encoded, timing(), StillDecodeLimits::default()).unwrap()
}

fn bytes(decoded: &DecodedStill) -> &[u8] {
    decoded.frame.payload().plane(0).unwrap().bytes()
}

fn pixel(decoded: &DecodedStill, x: u32, y: u32) -> [u8; 4] {
    let width = decoded.frame.payload().dimensions().width();
    let offset = ((y * width + x) * 4) as usize;
    bytes(decoded)[offset..offset + 4].try_into().unwrap()
}

fn size(decoded: &DecodedStill) -> (u32, u32) {
    let dimensions = decoded.frame.payload().dimensions();
    (dimensions.width(), dimensions.height())
}

fn assert_near(actual: [u8; 4], expected: [u8; 4], tolerance: u8) {
    for (actual_channel, expected_channel) in actual.iter().zip(expected) {
        assert!(
            actual_channel.abs_diff(expected_channel) <= tolerance,
            "{actual:?} is not within {tolerance} of {expected:?}"
        );
    }
}

/// A 2x1 picture: opaque orange, then a fully transparent pixel.
const RGBA_WITH_TRANSPARENT: [u8; 8] = [250, 120, 10, 255, 0, 0, 0, 0];

fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let mut encoded = Vec::new();
    PngEncoder::new(&mut encoded)
        .write_image(rgba, width, height, ExtendedColorType::Rgba8)
        .unwrap();
    encoded
}

fn encode_webp(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let mut encoded = Vec::new();
    WebPEncoder::new_lossless(&mut encoded)
        .write_image(rgba, width, height, ExtendedColorType::Rgba8)
        .unwrap();
    encoded
}

fn encode_tiff(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let mut encoded = std::io::Cursor::new(Vec::new());
    TiffEncoder::new(&mut encoded)
        .write_image(rgba, width, height, ExtendedColorType::Rgba8)
        .unwrap();
    encoded.into_inner()
}

fn encode_bmp(width: u32, height: u32, data: &[u8], color: ExtendedColorType) -> Vec<u8> {
    let mut encoded = Vec::new();
    BmpEncoder::new(&mut encoded)
        .write_image(data, width, height, color)
        .unwrap();
    encoded
}

fn encode_gif(frames: &[RgbaImage]) -> Vec<u8> {
    let mut encoded = Vec::new();
    {
        let mut encoder = GifEncoder::new(&mut encoded);
        for frame in frames {
            encoder
                .encode_frame(Frame::from_parts(
                    frame.clone(),
                    0,
                    0,
                    Delay::from_numer_denom_ms(100, 1),
                ))
                .unwrap();
        }
    }
    encoded
}

fn encode_jpeg(width: u32, height: u32, rgb: &[u8]) -> Vec<u8> {
    let mut encoded = Vec::new();
    JpegEncoder::new_with_quality(&mut encoded, 100)
        .write_image(rgb, width, height, ExtendedColorType::Rgb8)
        .unwrap();
    encoded
}

#[test]
fn sniffs_every_format_from_its_signature() {
    let cases: [(&[u8], StillFormat); 9] = [
        (b"RIFF\x10\0\0\0WEBPVP8L", StillFormat::Webp),
        (b"GIF87a..", StillFormat::Gif),
        (b"GIF89a..", StillFormat::Gif),
        (b"BM\0\0\0\0", StillFormat::Bmp),
        (b"II*\0\x08\0\0\0", StillFormat::Tiff),
        (b"MM\0*\0\0\0\x08", StillFormat::Tiff),
        (b"<svg xmlns='http://www.w3.org/2000/svg'/>", StillFormat::Svg),
        (
            b"\xef\xbb\xbf  <?xml version=\"1.0\"?>\n<!-- made by hand -->\n<!DOCTYPE svg PUBLIC \"-//W3C//DTD SVG 1.1//EN\" \"x\">\n<svg>",
            StillFormat::Svg,
        ),
        (
            b"<!DOCTYPE svg [ <!ENTITY a \"b\"> ]>\r\n<svg\n width=\"1\">",
            StillFormat::Svg,
        ),
    ];
    for (prefix, format) in cases {
        assert_eq!(sniff_still_format(prefix), Ok(format), "{prefix:?}");
    }
    assert_eq!(
        sniff_still_format(b"<!-- unterminated <svg>"),
        Err(StillDecodeError::UnsupportedFormat)
    );
    const { assert!(STILL_SNIFF_BYTES >= 512) };
}

#[test]
fn file_name_filter_matches_supported_extensions_case_insensitively() {
    for name in [
        "a.png", "B.JPG", "c.jpeg", "d.WebP", "e.gif", "f.bmp", "g.tif", "h.TIFF", "i.svg",
    ] {
        assert!(is_still_file_name(name), "{name}");
    }
    for name in ["png", ".png", "a.png.txt", "a.mp4", "noext", "a."] {
        assert!(!is_still_file_name(name), "{name}");
    }
    assert!(STILL_FILE_EXTENSIONS.contains(&"webp"));
    assert!(
        STILL_FILE_EXTENSIONS
            .iter()
            .all(|extension| *extension == extension.to_ascii_lowercase())
    );
}

#[test]
fn webp_tiff_and_png_preserve_exact_straight_alpha() {
    for (encoded, format) in [
        (encode_png(2, 1, &RGBA_WITH_TRANSPARENT), StillFormat::Png),
        (encode_webp(2, 1, &RGBA_WITH_TRANSPARENT), StillFormat::Webp),
        (encode_tiff(2, 1, &RGBA_WITH_TRANSPARENT), StillFormat::Tiff),
        (
            encode_bmp(2, 1, &RGBA_WITH_TRANSPARENT, ExtendedColorType::Rgba8),
            StillFormat::Bmp,
        ),
    ] {
        let decoded = decode(&encoded);
        assert_eq!(decoded.format, format);
        assert!(decoded.source_has_alpha, "{format}");
        assert!(!decoded.animated);
        assert_eq!(size(&decoded), (2, 1));
        assert_eq!(pixel(&decoded, 0, 0), [250, 120, 10, 255], "{format}");
        assert_eq!(pixel(&decoded, 1, 0)[3], 0, "{format}");
    }
}

#[test]
fn opaque_formats_report_no_alpha() {
    let rgb = [10, 200, 30, 40, 50, 60];
    for (encoded, format) in [
        (
            encode_bmp(2, 1, &rgb, ExtendedColorType::Rgb8),
            StillFormat::Bmp,
        ),
        (encode_jpeg(2, 1, &rgb), StillFormat::Jpeg),
        (
            encode_gif(&[RgbaImage::from_raw(1, 1, vec![10, 200, 30, 255]).unwrap()]),
            StillFormat::Gif,
        ),
    ] {
        let decoded = decode(&encoded);
        assert_eq!(decoded.format, format);
        assert!(!decoded.source_has_alpha, "{format}");
        assert!(
            bytes(&decoded)
                .as_chunks::<4>()
                .0
                .iter()
                .all(|pixel| pixel[3] == 255)
        );
    }
    let decoded = decode(&encode_bmp(2, 1, &rgb, ExtendedColorType::Rgb8));
    assert_eq!(bytes(&decoded), [10, 200, 30, 255, 40, 50, 60, 255]);
}

#[test]
fn gif_transparency_counts_as_alpha() {
    let frame = RgbaImage::from_raw(2, 1, vec![255, 0, 0, 255, 0, 0, 0, 0]).unwrap();
    let decoded = decode(&encode_gif(&[frame]));
    assert_eq!(decoded.format, StillFormat::Gif);
    assert!(decoded.source_has_alpha);
    assert!(!decoded.animated);
    assert_eq!(pixel(&decoded, 0, 0), [255, 0, 0, 255]);
    assert_eq!(pixel(&decoded, 1, 0)[3], 0);
}

#[test]
fn animated_gif_decodes_its_first_frame() {
    let red = RgbaImage::from_pixel(4, 2, image::Rgba([255, 0, 0, 255]));
    let blue = RgbaImage::from_pixel(4, 2, image::Rgba([0, 0, 255, 255]));
    let decoded = decode(&encode_gif(&[red, blue]));
    assert_eq!(decoded.format, StillFormat::Gif);
    assert!(decoded.animated);
    assert_eq!(size(&decoded), (4, 2));
    for y in 0..2 {
        for x in 0..4 {
            assert_eq!(pixel(&decoded, x, y), [255, 0, 0, 255]);
        }
    }
}

fn riff_chunk(kind: [u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut chunk = kind.to_vec();
    chunk.extend_from_slice(&u32::try_from(payload.len()).unwrap().to_le_bytes());
    chunk.extend_from_slice(payload);
    if payload.len() % 2 == 1 {
        chunk.push(0);
    }
    chunk
}

fn u24(value: u32) -> [u8; 3] {
    let bytes = value.to_le_bytes();
    [bytes[0], bytes[1], bytes[2]]
}

/// Builds an animated WebP from lossless still encodes of each frame.
fn animated_webp(width: u32, height: u32, frames: &[[u8; 4]]) -> Vec<u8> {
    let mut body = b"WEBP".to_vec();
    let mut vp8x = vec![0x12, 0, 0, 0];
    vp8x.extend_from_slice(&u24(width - 1));
    vp8x.extend_from_slice(&u24(height - 1));
    body.extend(riff_chunk(*b"VP8X", &vp8x));
    body.extend(riff_chunk(*b"ANIM", &[0, 0, 0, 0, 0, 0]));
    for color in frames {
        let still = encode_webp(width, height, &color.repeat((width * height) as usize));
        assert_eq!(&still[12..16], b"VP8L");
        let mut anmf = Vec::new();
        anmf.extend_from_slice(&u24(0));
        anmf.extend_from_slice(&u24(0));
        anmf.extend_from_slice(&u24(width - 1));
        anmf.extend_from_slice(&u24(height - 1));
        anmf.extend_from_slice(&u24(100));
        anmf.push(0b10);
        anmf.extend_from_slice(&still[12..]);
        body.extend(riff_chunk(*b"ANMF", &anmf));
    }
    let mut encoded = b"RIFF".to_vec();
    encoded.extend_from_slice(&u32::try_from(body.len()).unwrap().to_le_bytes());
    encoded.extend(body);
    encoded
}

#[test]
fn animated_webp_decodes_its_first_frame() {
    let encoded = animated_webp(3, 2, &[[0, 255, 0, 255], [0, 0, 255, 255]]);
    let decoded = decode(&encoded);
    assert_eq!(decoded.format, StillFormat::Webp);
    assert!(decoded.animated);
    assert_eq!(size(&decoded), (3, 2));
    assert_eq!(pixel(&decoded, 2, 1), [0, 255, 0, 255]);
}

/// A little-endian TIFF/EXIF block with one Orientation entry.
fn exif_orientation(orientation: u16) -> Vec<u8> {
    let mut exif = b"II".to_vec();
    exif.extend_from_slice(&42_u16.to_le_bytes());
    exif.extend_from_slice(&8_u32.to_le_bytes());
    exif.extend_from_slice(&1_u16.to_le_bytes());
    exif.extend_from_slice(&0x0112_u16.to_le_bytes());
    exif.extend_from_slice(&3_u16.to_le_bytes());
    exif.extend_from_slice(&1_u32.to_le_bytes());
    exif.extend_from_slice(&orientation.to_le_bytes());
    exif.extend_from_slice(&0_u16.to_le_bytes());
    exif.extend_from_slice(&0_u32.to_le_bytes());
    exif
}

/// A 16x8 JPEG, red on the left 8x8 block and blue on the right, with a
/// hand-written APP1 Exif segment spliced in after SOI.
fn jpeg_with_app1_orientation(orientation: u16) -> Vec<u8> {
    let mut rgb = Vec::new();
    for _ in 0..8 {
        for x in 0..16 {
            rgb.extend_from_slice(if x < 8 { &[255, 0, 0] } else { &[0, 0, 255] });
        }
    }
    let mut encoded = encode_jpeg(16, 8, &rgb);
    let mut payload = b"Exif\0\0".to_vec();
    payload.extend(exif_orientation(orientation));
    let mut app1 = vec![0xff, 0xe1];
    app1.extend_from_slice(&u16::try_from(payload.len() + 2).unwrap().to_be_bytes());
    app1.extend(payload);
    encoded.splice(2..2, app1);
    encoded
}

const RED: [u8; 4] = [255, 0, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];

#[test]
fn jpeg_app1_orientation_rotates_and_mirrors_pixels() {
    // 6: rotate 90 degrees clockwise; the left (red) half moves to the top.
    let decoded = decode(&jpeg_with_app1_orientation(6));
    assert!(decoded.orientation_applied);
    assert_eq!(
        (
            decoded.source_dimensions.width(),
            decoded.source_dimensions.height()
        ),
        (16, 8)
    );
    assert_eq!(size(&decoded), (8, 16));
    assert_near(pixel(&decoded, 4, 3), RED, 24);
    assert_near(pixel(&decoded, 4, 12), BLUE, 24);

    // 8: rotate 90 degrees counter-clockwise; the left half moves down.
    let decoded = decode(&jpeg_with_app1_orientation(8));
    assert_eq!(size(&decoded), (8, 16));
    assert_near(pixel(&decoded, 4, 3), BLUE, 24);
    assert_near(pixel(&decoded, 4, 12), RED, 24);

    // 2: mirror horizontally.
    let decoded = decode(&jpeg_with_app1_orientation(2));
    assert!(decoded.orientation_applied);
    assert_eq!(size(&decoded), (16, 8));
    assert_near(pixel(&decoded, 3, 4), BLUE, 24);
    assert_near(pixel(&decoded, 12, 4), RED, 24);

    // 1: untouched.
    let decoded = decode(&jpeg_with_app1_orientation(1));
    assert!(!decoded.orientation_applied);
    assert_near(pixel(&decoded, 3, 4), RED, 24);
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn png_chunk(kind: [u8; 4], data: &[u8]) -> Vec<u8> {
    let mut chunk = Vec::new();
    chunk.extend_from_slice(&u32::try_from(data.len()).unwrap().to_be_bytes());
    chunk.extend_from_slice(&kind);
    chunk.extend_from_slice(data);
    let mut covered = kind.to_vec();
    covered.extend_from_slice(data);
    chunk.extend_from_slice(&crc32(&covered).to_be_bytes());
    chunk
}

#[test]
fn png_exif_chunk_orientation_is_applied() {
    // 2x1: [A, B]; orientation 8 rotates counter-clockwise so B is on top.
    let mut encoded = encode_png(2, 1, &[10, 20, 30, 255, 200, 210, 220, 255]);
    encoded.splice(33..33, png_chunk(*b"eXIf", &exif_orientation(8)));
    let decoded = decode(&encoded);
    assert!(decoded.orientation_applied);
    assert_eq!(size(&decoded), (1, 2));
    assert_eq!(pixel(&decoded, 0, 0), [200, 210, 220, 255]);
    assert_eq!(pixel(&decoded, 0, 1), [10, 20, 30, 255]);
}

fn png_rgba_with_icc(rgba: &[u8], profile: &[u8]) -> Vec<u8> {
    let mut encoded = Vec::new();
    let mut encoder = PngEncoder::new(&mut encoded);
    encoder.set_icc_profile(profile.to_vec()).unwrap();
    encoder
        .write_image(rgba, 1, 1, ExtendedColorType::Rgba8)
        .unwrap();
    encoded
}

fn p3_to_srgb_reference(rgba: [u8; 4]) -> [u8; 4] {
    let transform = ColorProfile::new_display_p3()
        .create_transform_8bit(
            Layout::Rgba,
            &ColorProfile::new_srgb(),
            Layout::Rgba,
            TransformOptions::default(),
        )
        .unwrap();
    let mut output = [0; 4];
    transform.transform(&rgba, &mut output).unwrap();
    output
}

#[test]
fn display_p3_png_is_converted_to_srgb_preserving_alpha() {
    let p3 = ColorProfile::new_display_p3().encode().unwrap();
    // A saturated P3 green inside the sRGB-reachable range after clipping:
    // conversion pushes red and blue further down and keeps alpha.
    let source = [90, 200, 70, 128];
    let decoded = decode(&png_rgba_with_icc(&source, &p3));
    assert_eq!(decoded.color_profile, StillColorProfile::ConvertedToSrgb);
    let converted = pixel(&decoded, 0, 0);
    assert_eq!(converted[3], 128);
    assert_near(converted, p3_to_srgb_reference(source), 1);
    assert!(
        converted[0] + 10 < source[0],
        "red should drop toward the sRGB expectation: {converted:?}"
    );
}

#[test]
fn display_p3_jpeg_app2_profile_is_converted() {
    let p3 = ColorProfile::new_display_p3().encode().unwrap();
    let mut encoded = Vec::new();
    let mut encoder = JpegEncoder::new_with_quality(&mut encoded, 100);
    encoder.set_icc_profile(p3).unwrap();
    encoder
        .write_image(&[90, 200, 70].repeat(64), 8, 8, ExtendedColorType::Rgb8)
        .unwrap();
    let decoded = decode(&encoded);
    assert_eq!(decoded.color_profile, StillColorProfile::ConvertedToSrgb);
    assert_near(
        pixel(&decoded, 4, 4),
        p3_to_srgb_reference([90, 200, 70, 255]),
        6,
    );
}

#[test]
fn srgb_tagged_and_garbage_profiles_leave_pixels_untouched() {
    let source = [90, 200, 70, 200];
    let srgb = ColorProfile::new_srgb().encode().unwrap();
    let decoded = decode(&png_rgba_with_icc(&source, &srgb));
    assert_eq!(decoded.color_profile, StillColorProfile::EmbeddedSrgb);
    assert_eq!(pixel(&decoded, 0, 0), source);

    let mut garbage = srgb;
    garbage.truncate(60);
    let decoded = decode(&png_rgba_with_icc(&source, &garbage));
    assert_eq!(decoded.color_profile, StillColorProfile::IgnoredUnsupported);
    assert_eq!(pixel(&decoded, 0, 0), source);

    let limits = StillDecodeLimits {
        max_icc_bytes: 10,
        ..StillDecodeLimits::default()
    };
    assert!(matches!(
        decode_still(&png_rgba_with_icc(&source, &garbage), timing(), limits),
        Err(StillDecodeError::IccProfileTooLarge {
            required: 60,
            maximum: 10
        })
    ));
}

#[test]
fn grey_profile_on_a_grey_png_converts_to_neutral_srgb() {
    let gamma_10 = ColorProfile::new_gray_with_gamma(1.0).encode().unwrap();
    let mut encoded = Vec::new();
    let mut encoder = PngEncoder::new(&mut encoded);
    encoder.set_icc_profile(gamma_10).unwrap();
    encoder
        .write_image(&[64], 1, 1, ExtendedColorType::L8)
        .unwrap();
    let decoded = decode(&encoded);
    assert_eq!(decoded.color_profile, StillColorProfile::ConvertedToSrgb);
    let [red, green, blue, alpha] = pixel(&decoded, 0, 0);
    assert_eq!(alpha, 255);
    assert!(red.abs_diff(green) <= 1 && green.abs_diff(blue) <= 1);
    // Linear 0.25 is about 137 in sRGB.
    assert!(red.abs_diff(137) <= 3, "{red}");
}

const SVG: &str = r##"<?xml version="1.0" encoding="UTF-8"?>
<!-- left half red, right half empty -->
<svg xmlns="http://www.w3.org/2000/svg" width="20" height="10" viewBox="0 0 20 10">
  <rect x="0" y="0" width="10" height="10" fill="#ff0000"/>
  <text x="12" y="8">ignored without fonts</text>
  <image href="/etc/passwd" x="10" y="0" width="10" height="10"/>
</svg>"##;

#[test]
fn svg_renders_at_its_intrinsic_size_with_transparent_background() {
    let decoded = decode(SVG.as_bytes());
    assert_eq!(decoded.format, StillFormat::Svg);
    assert!(decoded.source_has_alpha);
    assert_eq!(size(&decoded), (20, 10));
    assert_eq!(
        (
            decoded.source_dimensions.width(),
            decoded.source_dimensions.height()
        ),
        (20, 10)
    );
    assert_eq!(pixel(&decoded, 2, 5), [255, 0, 0, 255]);
    assert_eq!(pixel(&decoded, 15, 5), [0, 0, 0, 0]);
}

#[test]
fn svg_rasterises_directly_into_the_output_viewport() {
    let output = VideoDimensions::new(80, 80).unwrap();
    let decoded = decode_still_for_output(
        SVG.as_bytes(),
        timing(),
        StillDecodeLimits::default(),
        output,
        OutputScaling::Fit,
    )
    .unwrap();
    assert_eq!(size(&decoded), (80, 80));
    // Fit places the 2:1 picture at y 20..60.
    assert_eq!(pixel(&decoded, 10, 40), [255, 0, 0, 255]);
    assert_eq!(pixel(&decoded, 39, 59), [255, 0, 0, 255]);
    assert_eq!(pixel(&decoded, 39, 20), [255, 0, 0, 255]);
    assert_eq!(pixel(&decoded, 10, 19), [0, 0, 0, 0]);
    assert_eq!(pixel(&decoded, 10, 60), [0, 0, 0, 0]);
    assert_eq!(pixel(&decoded, 60, 40), [0, 0, 0, 0]);

    let stretched = decode_still_for_output(
        SVG.as_bytes(),
        timing(),
        StillDecodeLimits::default(),
        VideoDimensions::new(30, 30).unwrap(),
        OutputScaling::Stretch,
    )
    .unwrap();
    assert_eq!(pixel(&stretched, 5, 0), [255, 0, 0, 255]);
    assert_eq!(pixel(&stretched, 5, 29), [255, 0, 0, 255]);
    assert_eq!(pixel(&stretched, 25, 15), [0, 0, 0, 0]);
}

#[test]
fn malformed_svg_and_oversized_svg_are_typed_errors() {
    assert_eq!(
        decode_still(b"<svg><g></svg>", timing(), StillDecodeLimits::default()),
        Err(StillDecodeError::CorruptInput {
            format: StillFormat::Svg
        })
    );
    let limits = StillDecodeLimits {
        max_width: 10,
        ..StillDecodeLimits::default()
    };
    assert_eq!(
        decode_still(SVG.as_bytes(), timing(), limits),
        Err(StillDecodeError::WidthTooLarge {
            actual: 20,
            maximum: 10
        })
    );
}

#[test]
fn raster_decode_for_output_fits_and_keeps_source_facts() {
    let encoded = encode_png(2, 1, &RGBA_WITH_TRANSPARENT);
    let decoded = decode_still_for_output(
        &encoded,
        timing(),
        StillDecodeLimits::default(),
        VideoDimensions::new(4, 4).unwrap(),
        OutputScaling::Fit,
    )
    .unwrap();
    assert_eq!(size(&decoded), (4, 4));
    assert_eq!(decoded.source_dimensions.width(), 2);
    assert!(decoded.source_has_alpha);
    assert_eq!(pixel(&decoded, 0, 0), [0, 0, 0, 0]);
    // Only the opaque pixel carries colour, so un-premultiplying restores it
    // exactly even where the filter mixes in the transparent neighbour.
    let edge = pixel(&decoded, 0, 1);
    assert!(edge[3] > 128, "{edge:?}");
    assert_eq!(edge[..3], [250, 120, 10]);
    assert_eq!(decoded.frame.timing(), timing());

    let same = decode_still_for_output(
        &encoded,
        timing(),
        StillDecodeLimits::default(),
        VideoDimensions::new(2, 1).unwrap(),
        OutputScaling::Fill,
    )
    .unwrap();
    assert_eq!(bytes(&same), RGBA_WITH_TRANSPARENT);
}

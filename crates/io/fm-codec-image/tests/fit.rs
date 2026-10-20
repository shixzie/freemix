//! Geometry and resampling of `fit_still`.

#![allow(
    clippy::cast_possible_truncation,
    reason = "fixtures deliberately wrap coordinates into byte channels"
)]

use std::num::NonZeroU128;
use std::time::Instant;

use fm_codec_image::{StillDecodeError, fit_still};
use fm_frame::{
    AlphaMode, ChromaLocation, ClockDomainId, ColorMetadata, ColorPrimaries, CpuVideoFrame,
    CpuVideoPayload, CpuVideoPlane, MatrixCoefficients, MediaTimestamp, MediaTiming,
    NormalizedDuration, NormalizedTimestamp, OriginalTimestamp, PixelFormat, SequenceNumber,
    SignalRange, TimeBase, TransferFunction, VideoDimensions, VideoFrameMetadata,
};
use fm_types::OutputScaling;

fn timing() -> MediaTiming {
    let time_base = TimeBase::new(1, 90_000).unwrap();
    MediaTiming::new(
        OriginalTimestamp::new(MediaTimestamp::new(7), time_base),
        NormalizedTimestamp::from_nanos(11),
        NormalizedDuration::from_nanos(40_000_000).unwrap(),
        ClockDomainId::new(NonZeroU128::new(5).unwrap()),
        SequenceNumber::new(9),
    )
    .unwrap()
}

fn metadata() -> VideoFrameMetadata {
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

fn dims(width: u32, height: u32) -> VideoDimensions {
    VideoDimensions::new(width, height).unwrap()
}

fn frame(width: u32, height: u32, rgba: Vec<u8>) -> CpuVideoFrame {
    let plane = CpuVideoPlane::new(width as usize * 4, rgba).unwrap();
    let payload =
        CpuVideoPayload::new(PixelFormat::Rgba8, dims(width, height), vec![plane]).unwrap();
    CpuVideoFrame::new(timing(), payload)
        .with_metadata(metadata())
        .unwrap()
}

fn solid(width: u32, height: u32, rgba: [u8; 4]) -> CpuVideoFrame {
    frame(width, height, rgba.repeat((width * height) as usize))
}

/// Every pixel distinct: (x, y, x ^ y, 255).
fn numbered(width: u32, height: u32) -> CpuVideoFrame {
    let mut rgba = Vec::new();
    for y in 0..height {
        for x in 0..width {
            rgba.extend_from_slice(&[x as u8, y as u8, (x ^ y) as u8, 255]);
        }
    }
    frame(width, height, rgba)
}

fn pixel(frame: &CpuVideoFrame, x: u32, y: u32) -> [u8; 4] {
    let width = frame.payload().dimensions().width();
    let offset = ((y * width + x) * 4) as usize;
    frame.payload().plane(0).unwrap().bytes()[offset..offset + 4]
        .try_into()
        .unwrap()
}

/// Asserts alpha is opaque exactly inside the viewport and every pixel
/// outside it is transparent black.
fn assert_covers_exactly_viewport(
    source: &CpuVideoFrame,
    fitted: &CpuVideoFrame,
    scaling: OutputScaling,
) {
    let source_dims = source.payload().dimensions();
    let output = fitted.payload().dimensions();
    let viewport = scaling
        .viewport(
            source_dims.width(),
            source_dims.height(),
            output.width(),
            output.height(),
        )
        .unwrap();
    for y in 0..output.height() {
        for x in 0..output.width() {
            let inside = i64::from(x) >= i64::from(viewport.x)
                && i64::from(x) < i64::from(viewport.x) + i64::from(viewport.width)
                && i64::from(y) >= i64::from(viewport.y)
                && i64::from(y) < i64::from(viewport.y) + i64::from(viewport.height);
            let value = pixel(fitted, x, y);
            if inside {
                assert_eq!(value[3], 255, "{scaling} ({x}, {y}) should be opaque");
            } else {
                assert_eq!(value, [0, 0, 0, 0], "{scaling} ({x}, {y}) should be clear");
            }
        }
    }
}

#[test]
fn fit_letterboxes_with_transparent_bars_and_exact_edges() {
    let source = solid(4, 2, [200, 30, 40, 255]);
    let fitted = fit_still(&source, dims(8, 8), OutputScaling::Fit).unwrap();
    assert_eq!(fitted.payload().dimensions(), dims(8, 8));
    assert_eq!(fitted.payload().plane(0).unwrap().stride(), 32);
    assert_eq!(fitted.timing(), timing());
    assert_eq!(fitted.metadata(), Some(metadata()));
    assert_covers_exactly_viewport(&source, &fitted, OutputScaling::Fit);
    assert_eq!(pixel(&fitted, 0, 2), [200, 30, 40, 255]);
    assert_eq!(pixel(&fitted, 7, 5), [200, 30, 40, 255]);
}

#[test]
fn every_policy_covers_exactly_its_viewport_at_odd_sizes() {
    for (source_size, output_size) in [
        ((3, 5), (7, 4)),
        ((5, 3), (4, 7)),
        ((7, 7), (3, 9)),
        ((2, 9), (11, 5)),
        ((13, 3), (5, 5)),
    ] {
        let source = solid(source_size.0, source_size.1, [10, 220, 90, 255]);
        for scaling in OutputScaling::ALL {
            let fitted = fit_still(&source, dims(output_size.0, output_size.1), scaling).unwrap();
            assert_covers_exactly_viewport(&source, &fitted, scaling);
            for y in 0..output_size.1 {
                for x in 0..output_size.0 {
                    let value = pixel(&fitted, x, y);
                    if value[3] != 0 {
                        assert_eq!(value, [10, 220, 90, 255], "{scaling} ({x}, {y})");
                    }
                }
            }
        }
    }
}

#[test]
fn fill_and_stretch_cover_the_whole_output() {
    let source = numbered(4, 2);
    for scaling in [OutputScaling::Fill, OutputScaling::Stretch] {
        let fitted = fit_still(&source, dims(6, 6), scaling).unwrap();
        for y in 0..6 {
            for x in 0..6 {
                assert_eq!(pixel(&fitted, x, y)[3], 255, "{scaling} ({x}, {y})");
            }
        }
    }
    // Fill of a 2:1 picture into a square keeps the centre columns: the
    // leftmost output column samples near source column 1, not 0.
    let wide = frame(
        4,
        1,
        [
            [255, 0, 0, 255],
            [0, 255, 0, 255],
            [0, 255, 0, 255],
            [0, 0, 255, 255],
        ]
        .concat(),
    );
    let filled = fit_still(&wide, dims(64, 32), OutputScaling::Fill).unwrap();
    assert_eq!(pixel(&filled, 32, 16), [0, 255, 0, 255]);
}

#[test]
fn one_to_one_copies_and_crops_exactly() {
    let source = numbered(6, 6);
    let cropped = fit_still(&source, dims(2, 2), OutputScaling::OneToOne).unwrap();
    for y in 0..2 {
        for x in 0..2 {
            assert_eq!(pixel(&cropped, x, y), pixel(&source, x + 2, y + 2));
        }
    }

    let small = numbered(2, 3);
    let bordered = fit_still(&small, dims(5, 6), OutputScaling::OneToOne).unwrap();
    assert_covers_exactly_viewport(&small, &bordered, OutputScaling::OneToOne);
    // Origin is ((5 - 2) / 2, (6 - 3) / 2) = (1, 1).
    for y in 0..3 {
        for x in 0..2 {
            assert_eq!(pixel(&bordered, x + 1, y + 1), pixel(&small, x, y));
        }
    }

    // Oversized on one axis and undersized on the other.
    let tall = numbered(3, 9);
    let mixed = fit_still(&tall, dims(7, 4), OutputScaling::OneToOne).unwrap();
    assert_eq!(pixel(&mixed, 2, 0), pixel(&tall, 0, 2));
    assert_eq!(pixel(&mixed, 4, 3), pixel(&tall, 2, 5));
    assert_eq!(pixel(&mixed, 1, 0), [0, 0, 0, 0]);
    assert_eq!(pixel(&mixed, 5, 3), [0, 0, 0, 0]);
}

#[test]
fn same_size_is_an_exact_copy_including_hidden_rgb() {
    let rgba = vec![1, 2, 3, 0, 40, 50, 60, 127, 70, 80, 90, 255, 9, 8, 7, 1];
    let source = frame(2, 2, rgba.clone());
    for scaling in OutputScaling::ALL {
        let fitted = fit_still(&source, dims(2, 2), scaling).unwrap();
        assert_eq!(fitted.payload().plane(0).unwrap().bytes(), rgba);
    }
}

#[test]
fn resampling_is_premultiplied_so_transparent_colour_never_fringes() {
    let source = frame(2, 1, vec![255, 0, 0, 255, 0, 255, 0, 0]);
    for (output, scaling) in [
        (dims(9, 1), OutputScaling::Stretch),
        (dims(16, 5), OutputScaling::Fill),
        (dims(7, 7), OutputScaling::Fit),
    ] {
        let fitted = fit_still(&source, output, scaling).unwrap();
        let mut partially_transparent = false;
        for y in 0..output.height() {
            for x in 0..output.width() {
                let [red, green, blue, alpha] = pixel(&fitted, x, y);
                if alpha == 0 {
                    assert_eq!([red, green, blue], [0, 0, 0]);
                    continue;
                }
                partially_transparent |= alpha < 255;
                assert_eq!(
                    [red, green, blue],
                    [255, 0, 0],
                    "{scaling} ({x}, {y}) alpha {alpha}"
                );
            }
        }
        assert!(partially_transparent, "{scaling} should blend the edge");
    }
}

#[test]
fn downscaling_averages_detail() {
    // A one-pixel checkerboard averages to mid grey when minified 4x.
    let mut rgba = Vec::new();
    for y in 0..16u32 {
        for x in 0..16u32 {
            let value = if (x + y) % 2 == 0 { 255 } else { 0 };
            rgba.extend_from_slice(&[value, value, value, 255]);
        }
    }
    let fitted = fit_still(&frame(16, 16, rgba), dims(4, 4), OutputScaling::Stretch).unwrap();
    for y in 0..4 {
        for x in 0..4 {
            let [red, green, blue, alpha] = pixel(&fitted, x, y);
            assert_eq!(alpha, 255);
            assert!(red.abs_diff(128) <= 12, "{red}");
            assert_eq!((red, red), (green, blue));
        }
    }
}

#[test]
fn rejects_non_rgba_input_and_oversized_output() {
    let payload = CpuVideoPayload::allocate(PixelFormat::Nv12, dims(4, 4)).unwrap();
    let nv12 = CpuVideoFrame::new(timing(), payload);
    assert_eq!(
        fit_still(&nv12, dims(8, 8), OutputScaling::Fit),
        Err(StillDecodeError::FitUnsupportedPixelFormat {
            format: PixelFormat::Nv12
        })
    );
    let source = solid(2, 2, [1, 2, 3, 255]);
    assert_eq!(
        fit_still(
            &source,
            dims(CpuVideoPayload::MAX_WIDTH + 1, 2),
            OutputScaling::Fit
        ),
        Err(StillDecodeError::FitOutputTooLarge {
            width: CpuVideoPayload::MAX_WIDTH + 1,
            height: 2
        })
    );
    assert_eq!(
        fit_still(&source, dims(16_384, 16_384), OutputScaling::Fit),
        Err(StillDecodeError::FitOutputTooLarge {
            width: 16_384,
            height: 16_384
        })
    );
}

/// Run with `cargo test --release -p fm-codec-image --test fit -- --ignored
/// --nocapture` to measure a 24-megapixel photo fitted to 1080p.
#[test]
#[ignore = "timing benchmark; meaningful only in release builds"]
fn large_photo_fit_timing() {
    let (width, height) = (6000u32, 4000u32);
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            rgba.extend_from_slice(&[x as u8, y as u8, (x ^ y) as u8, 255]);
        }
    }
    let source = frame(width, height, rgba);
    for scaling in OutputScaling::ALL {
        let start = Instant::now();
        let fitted = fit_still(&source, dims(1920, 1080), scaling).unwrap();
        println!("{scaling}: {:?}", start.elapsed());
        assert_eq!(fitted.payload().dimensions(), dims(1920, 1080));
    }
}

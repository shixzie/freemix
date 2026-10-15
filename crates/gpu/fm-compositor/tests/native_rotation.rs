//! GX-011 arbitrary-angle layer rotation: native GPU against the CPU
//! reference.
//!
//! Both executors draw the same untilted footprint, inverse-map each output
//! pixel centre with the shared Q30 sine and cosine, and mix four footprint
//! texels with Q8 weights. The CPU reference runs on the working-space
//! (linear) values quantized to 8 bits, so the two agree up to float versus
//! integer rounding. Runs on any wgpu adapter (including lavapipe); without
//! one the tests skip, unless `FM_REQUIRE_NATIVE_MEDIA=1` makes that a
//! failure.

#![cfg(feature = "native-wgpu")]

use std::{
    future::Future,
    num::NonZeroU128,
    pin::pin,
    sync::Arc,
    task::{Context, Poll, Wake, Waker},
    thread,
};

use fm_color::{ColorPipeline, NativeImportNormalizer, working_color_metadata};
use fm_compositor::{
    CpuSourceFrame, CropRect, NativeCompositionRenderer, NativeSourceFrame, OutputTarget, RectMask,
    Rgba8, Rotation, Scene, SourceId, SourceLayer, Transform, compile_scene, execute_cpu,
};
use fm_frame::{
    AlphaMode, ChromaLocation, ClockDomainId, ColorMetadata, ColorPrimaries, CpuVideoFrame,
    CpuVideoPayload, CpuVideoPlane, MatrixCoefficients, MediaTimestamp, MediaTiming,
    NormalizedDuration, NormalizedTimestamp, OriginalTimestamp, PixelFormat, SequenceNumber,
    SignalRange, TimeBase, TransferFunction, VideoDimensions, VideoFrameMetadata,
};
use fm_gpu::{NativeBackend, NativeContext, TextureFormat};
use fm_video::ImageFrame;
use half::f16;

/// Largest per-component difference, in 8-bit codes.
const MAX_ERROR: f32 = 2.0;
/// Mean per-component difference, in 8-bit codes.
const MEAN_ERROR: f64 = 0.25;

struct ThreadWaker(thread::Thread);

impl Wake for ThreadWaker {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark();
    }
}

fn block_on<F: Future>(future: F) -> F::Output {
    let waker = Waker::from(Arc::new(ThreadWaker(thread::current())));
    let mut context = Context::from_waker(&waker);
    let mut future = pin!(future);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(output) => return output,
            Poll::Pending => thread::park(),
        }
    }
}

/// Any native adapter, or `None` (skip) when none is available.
fn native_context(test: &str) -> Option<NativeContext> {
    match block_on(NativeContext::new([
        NativeBackend::Vulkan,
        NativeBackend::Metal,
        NativeBackend::Dx12,
        NativeBackend::Gl,
    ])) {
        Ok(context) => {
            eprintln!("{test}: adapter {:?}", context.adapter_info());
            Some(context)
        }
        Err(error) => {
            assert!(
                std::env::var("FM_REQUIRE_NATIVE_MEDIA").as_deref() != Ok("1"),
                "FM_REQUIRE_NATIVE_MEDIA=1: {error}"
            );
            eprintln!("{test} skipped: no native adapter ({error})");
            None
        }
    }
}

fn timing() -> MediaTiming {
    MediaTiming::new(
        OriginalTimestamp::new(MediaTimestamp::new(1), TimeBase::new(1, 1_000).unwrap()),
        NormalizedTimestamp::from_nanos(1_000_000),
        NormalizedDuration::from_nanos(1_000_000).unwrap(),
        ClockDomainId::new(NonZeroU128::new(23).unwrap()),
        SequenceNumber::new(1),
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

fn source_frame(width: u32, height: u32, pixels: &[u8]) -> CpuVideoFrame {
    let payload = CpuVideoPayload::new(
        PixelFormat::Rgba8,
        VideoDimensions::new(width, height).unwrap(),
        vec![CpuVideoPlane::new(width as usize * 4, pixels.to_vec()).unwrap()],
    )
    .unwrap();
    CpuVideoFrame::new(timing(), payload)
        .with_metadata(metadata())
        .unwrap()
}

/// The source as the native path sees it: linear, premultiplied, quantized.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn working_image(source: &CpuVideoFrame) -> ImageFrame {
    let pipeline = ColorPipeline::new(metadata().color(), working_color_metadata());
    let decoded = pipeline.decode_cpu_payload(source.payload()).unwrap().frame;
    let bytes = decoded
        .pixels()
        .iter()
        .flat_map(|pixel| {
            [pixel.r, pixel.g, pixel.b, pixel.a]
                .map(|component| (component.clamp(0.0, 1.0) * 255.0).round() as u8)
        })
        .collect::<Vec<_>>();
    ImageFrame::new(
        decoded.width(),
        decoded.height(),
        decoded.width() as usize * 4,
        bytes,
    )
    .unwrap()
}

/// A 7x5 picture with distinct, partly transparent texels.
#[allow(clippy::cast_possible_truncation)]
fn pattern() -> (u32, u32, Vec<u8>) {
    let (width, height) = (7_u32, 5_u32);
    let mut pixels = Vec::new();
    for y in 0..height {
        for x in 0..width {
            let r = (x * 36) as u8;
            let g = (y * 60) as u8;
            let b = ((x + y) * 20) as u8;
            let a = if (x + y) % 4 == 0 { 160 } else { 255 };
            pixels.extend_from_slice(&[r, g, 255 - b, a]);
        }
    }
    (width, height, pixels)
}

struct Case {
    name: &'static str,
    layer: SourceLayer,
}

fn cases() -> Vec<Case> {
    let source = SourceId::new(1);
    let layer = |x, y, width, height, rotation| {
        SourceLayer::new(source, 0, Transform::new(x, y, width, height, rotation))
            .with_alpha_mode(AlphaMode::Premultiplied)
    };
    let mut cases = Vec::new();
    for (name, millidegrees) in [
        ("1 degree", 1_000),
        ("15 degrees", 15_000),
        ("30 degrees", 30_000),
        ("45 degrees", 45_000),
        ("-33.333 degrees", -33_333),
        ("100 degrees", 100_000),
        ("200 degrees", 200_000),
        ("359.999 degrees", 359_999),
    ] {
        cases.push(Case {
            name,
            layer: layer(6, 4, 14, 9, Rotation::Deg0).with_rotation_millidegrees(millidegrees),
        });
    }
    cases.push(Case {
        name: "quarter turn plus 20 degrees, crop, mask, opacity",
        layer: layer(5, 3, 13, 10, Rotation::Deg90)
            .with_rotation_millidegrees(20_000)
            .with_crop(CropRect::new(1, 1, 5, 4))
            .with_mask(RectMask::new(1, 0, 3, 3).inverted(true))
            .with_opacity(170),
    });
    cases.push(Case {
        name: "inset border at -12 degrees",
        layer: layer(4, 6, 16, 8, Rotation::Deg180)
            .with_rotation_millidegrees(-12_000)
            .with_inset_border(2),
    });
    cases.push(Case {
        name: "clipped at the canvas edge",
        layer: layer(-6, 14, 18, 12, Rotation::Deg270).with_rotation_millidegrees(37_500),
    });
    cases.push(Case {
        name: "a whole turn draws untilted",
        layer: layer(3, 2, 11, 7, Rotation::Deg0).with_rotation_millidegrees(360_000),
    });
    cases
}

#[test]
fn tilted_layers_match_the_cpu_reference() {
    let Some(context) = native_context("tilted_layers_match_the_cpu_reference") else {
        return;
    };
    block_on(async {
        let normalizer = NativeImportNormalizer::new(&context).await.unwrap();
        let renderer = NativeCompositionRenderer::new(&context).await.unwrap();
        let (width, height, pixels) = pattern();
        let source = source_frame(width, height, &pixels);
        let cpu_source = working_image(&source);
        let working = normalizer.normalize(&context, &source).await.unwrap();
        for case in cases() {
            let mut scene = Scene::new(26, 22, Rgba8::new(10, 20, 30, 255)).unwrap();
            scene.push_layer(case.layer.clone());
            let (plan, _) = compile_scene(&scene, OutputTarget::Program).unwrap();
            let expected =
                execute_cpu(&plan, &[CpuSourceFrame::new(SourceId::new(1), &cpu_source)]).unwrap();
            let output = renderer
                .render(
                    &context,
                    &plan,
                    &[NativeSourceFrame::new(SourceId::new(1), working.texture())],
                )
                .await
                .unwrap();
            let readback = context.readback(&output).await.unwrap();
            assert_eq!(readback.format, TextureFormat::Rgba16Float);
            let mut max = 0.0_f32;
            let mut sum = 0.0_f64;
            let mut count = 0_u32;
            for (index, (actual, expected)) in readback
                .bytes
                .as_chunks::<8>()
                .0
                .iter()
                .zip(expected.pixels().as_chunks::<4>().0)
                .enumerate()
            {
                for (component, (actual, expected)) in
                    actual.as_chunks::<2>().0.iter().zip(expected).enumerate()
                {
                    let actual = f16::from_bits(u16::from_le_bytes(*actual));
                    let error = (actual.to_f32() * 255.0 - f32::from(*expected)).abs();
                    assert!(
                        error <= MAX_ERROR,
                        "{}: pixel {index} component {component}: GPU {} CPU {expected}",
                        case.name,
                        actual.to_f32() * 255.0
                    );
                    max = max.max(error);
                    sum += f64::from(error);
                    count += 1;
                }
            }
            let mean = sum / f64::from(count);
            eprintln!(
                "{}: max {max:.3}/255, mean {mean:.4}/255 over {count} components",
                case.name
            );
            assert!(mean <= MEAN_ERROR, "{}: mean error {mean}", case.name);
        }
    });
}

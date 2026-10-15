//! SW-002 transition family: the native GPU renderer against the CPU
//! reference ([`execute_transition`], i.e. `fm_video::render_transition`).
//!
//! Both executors run on the working-space (linear, premultiplied) values;
//! the CPU reference sees them quantized to 8 bits while the GPU keeps f16,
//! so integer-copy kinds agree to that quantization and sampling kinds to
//! float versus integer rounding. Every [`TransitionPlan`] kind is covered on
//! an odd-sized fixture with partly transparent texels. Runs on any wgpu
//! adapter (including lavapipe); without one the tests skip, unless
//! `FM_REQUIRE_NATIVE_MEDIA=1` makes that a failure.

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
use fm_compositor::{NativeTransitionRenderer, TransitionKind, TransitionPlan, execute_transition};
use fm_frame::{
    AlphaMode, ChromaLocation, ClockDomainId, ColorMetadata, ColorPrimaries, CpuVideoFrame,
    CpuVideoPayload, CpuVideoPlane, MatrixCoefficients, MediaTimestamp, MediaTiming,
    NormalizedDuration, NormalizedTimestamp, OriginalTimestamp, PixelFormat, SequenceNumber,
    SignalRange, TimeBase, TransferFunction, VideoDimensions, VideoFrameMetadata,
};
use fm_gpu::{NativeBackend, NativeContext, TextureFormat};
use fm_video::ImageFrame;
use half::f16;

const WIDTH: u32 = 37;
const HEIGHT: u32 = 23;

/// Progress values, including both endpoints.
const PROGRESS: [(u32, u32); 7] = [(0, 1), (1, 7), (1, 3), (1, 2), (5, 8), (6, 7), (1, 1)];

/// Per-component error budgets in 8-bit codes.
struct Budget {
    /// Every component within this.
    max: f32,
    /// Mean over every component of every progress value.
    mean: f64,
    /// Components above `soft` (coverage-edge rounding) are counted...
    soft: f32,
    /// ...and may number at most this many over all progress values.
    outliers: usize,
}

/// Integer copies agree to the input quantization alone: the CPU reference's
/// 8-bit working image against the normalizer's f16 texture (Cut measures
/// the same 0.61 on lavapipe).
const EXACT: Budget = Budget {
    max: 0.65,
    mean: 0.22,
    soft: 0.65,
    outliers: 0,
};

/// The float crossfade of f16 values against the rounded integer crossfade
/// of quantized values.
const FADE: Budget = Budget {
    max: 1.2,
    mean: 0.28,
    soft: 1.2,
    outliers: 0,
};

/// Bilinear kinds: float coordinates and weights against Q8 integers, plus
/// the CPU's rounding of each sample before over or crossfade. A Q8 floor
/// landing on the other side of a 1/256 boundary moves a weight by 1/256, so
/// a few coverage-edge components may exceed 1.5 codes.
const BILINEAR: Budget = Budget {
    max: 2.0,
    mean: 0.35,
    soft: 1.5,
    outliers: 8,
};

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

fn source_frame(pixels: &[u8]) -> CpuVideoFrame {
    let payload = CpuVideoPayload::new(
        PixelFormat::Rgba8,
        VideoDimensions::new(WIDTH, HEIGHT).unwrap(),
        vec![CpuVideoPlane::new(WIDTH as usize * 4, pixels.to_vec()).unwrap()],
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

/// Distinct straight-alpha texels, some partly transparent, with gradients
/// steep enough that a wrong tap or weight shows.
#[allow(clippy::cast_possible_truncation)]
fn pattern(seed: u32) -> Vec<u8> {
    let mut pixels = Vec::new();
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let r = ((x * 7 + seed * 50) % 256) as u8;
            let g = ((y * 11 + seed * 90) % 256) as u8;
            let b = (((x + y) * 5 + seed * 30) % 256) as u8;
            let a = match (x + 2 * y + seed) % 5 {
                0 => 96,
                1 => 200,
                _ => 255,
            };
            pixels.extend_from_slice(&[r, g, b, a]);
        }
    }
    pixels
}

const KINDS: [(TransitionKind, Budget); 14] = [
    (TransitionKind::Cut, EXACT),
    (TransitionKind::Fade, FADE),
    (TransitionKind::AlphaFade, FADE),
    (TransitionKind::Wipe, EXACT),
    (TransitionKind::VerticalWipe, EXACT),
    (TransitionKind::Slide, EXACT),
    (TransitionKind::VerticalSlide, EXACT),
    (TransitionKind::Zoom, EXACT),
    (TransitionKind::Merge, EXACT),
    (TransitionKind::Fly, BILINEAR),
    (TransitionKind::FlyRotate, BILINEAR),
    (TransitionKind::Cube, BILINEAR),
    (TransitionKind::CubeZoom, BILINEAR),
    (TransitionKind::CrossZoom, BILINEAR),
];

#[test]
fn every_transition_kind_matches_the_cpu_reference() {
    let Some(context) = native_context("every_transition_kind_matches_the_cpu_reference") else {
        return;
    };
    block_on(async {
        let normalizer = NativeImportNormalizer::new(&context).await.unwrap();
        let renderer = NativeTransitionRenderer::new(&context).await.unwrap();
        let (from, to) = (source_frame(&pattern(0)), source_frame(&pattern(1)));
        let (cpu_from, cpu_to) = (working_image(&from), working_image(&to));
        let gpu_from = normalizer.normalize(&context, &from).await.unwrap();
        let gpu_to = normalizer.normalize(&context, &to).await.unwrap();
        for (kind, budget) in KINDS {
            let mut max = 0.0_f32;
            let mut sum = 0.0_f64;
            let mut count = 0_u32;
            let mut outliers = 0_usize;
            for (numerator, denominator) in PROGRESS {
                let plan = TransitionPlan::compile(kind, numerator, denominator).unwrap();
                let expected = execute_transition(plan, &cpu_from, &cpu_to).unwrap();
                let output = renderer
                    .render(&context, plan, gpu_from.texture(), gpu_to.texture())
                    .await
                    .unwrap();
                let readback = context.readback(&output).await.unwrap();
                assert_eq!(readback.format, TextureFormat::Rgba16Float);
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
                        let actual = f16::from_bits(u16::from_le_bytes(*actual)).to_f32() * 255.0;
                        let error = (actual - f32::from(*expected)).abs();
                        assert!(
                            error <= budget.max,
                            "{kind:?} at {numerator}/{denominator}: pixel ({}, {}) component \
                             {component}: GPU {actual} CPU {expected}",
                            index % WIDTH as usize,
                            index / WIDTH as usize,
                        );
                        if error > budget.soft {
                            outliers += 1;
                        }
                        max = max.max(error);
                        sum += f64::from(error);
                        count += 1;
                    }
                }
            }
            let mean = sum / f64::from(count);
            eprintln!(
                "{kind:?}: max {max:.3}/255, mean {mean:.4}/255, {outliers} components above \
                 {:.2} over {count} components",
                budget.soft
            );
            assert!(mean <= budget.mean, "{kind:?}: mean error {mean}");
            assert!(
                outliers <= budget.outliers,
                "{kind:?}: {outliers} components above {}",
                budget.soft
            );
        }
    });
}

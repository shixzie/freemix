//! Native layer preparation parity: chroma/luma keys, rect masks, and the
//! CPU reference effects against `execute_cpu`.
//!
//! Runs on any available wgpu adapter (including software Vulkan such as
//! lavapipe). Without an adapter the tests skip, unless
//! `FM_REQUIRE_NATIVE_MEDIA=1` makes that a failure.
//!
//! Scenes compose over a transparent background, so the native output is
//! compared without blending differences: each native pixel is straightened
//! and encoded to sRGB 8-bit BT.709; its alpha is compared with the CPU alpha
//! and its colour, premultiplied by the CPU alpha in the 8-bit domain the CPU
//! uses, with the CPU's premultiplied colour.
//!
//! Error budget. The preparation pass decodes the binary16 working texture
//! back to 8-bit codes and then runs the CPU's exact integer formulas, so the
//! only error source is that decode: binary16 BT.2020 cannot always return a
//! dark channel next to a bright one (a saturated green's red, say) to its
//! exact code, and the import/output round trip alone is up to 2 codes there
//! (the same bound as the SW-019 grade parity tests). Hence:
//!
//! - effects: colour within [`MAX_ERROR`] (2) codes, alpha exact (flip moves
//!   alpha, colour effects leave it alone);
//! - keys: a decoded code that is off by one moves the key ramp by
//!   `255 / softness`, so alpha is held to [`key_budget`]
//!   (`1 + 2 * ceil(255 / softness)`), and spill (which follows key alpha) to
//!   [`KEY_MAX_ERROR`] colour codes;
//! - grades add the grade pass's own LUT interpolation error
//!   ([`GRADED_MAX_ERROR`]).
//!
//! Every case is also held to a small mean error, so those maxima stay rare
//! edge pixels. Measured on lavapipe (llvmpipe): effects max 2 colour codes
//! with exact alpha; keys max 4 colour codes (spill on a red-dominant key)
//! and max 7 alpha codes (one decoded luma code on a 40-code ramp), with mean
//! errors below 0.07 codes; graded keyed layers max 1 code.

#![cfg(feature = "native-wgpu")]

use std::{
    future::Future,
    num::NonZeroU128,
    pin::pin,
    sync::Arc,
    task::{Context, Poll, Wake, Waker},
    thread,
};

use fm_color::{NativeImportNormalizer, NativeWorkingFrame, srgb_from_linear};
use fm_compositor::{
    AlphaMode, ChromaKey, CompiledGrade, CpuSourceFrame, CropRect, Effect, Key, LumaKey,
    NativeCompositionRenderer, NativeSourceFrame, OutputTarget, RectMask, Rgba8, Rotation, Scene,
    SourceId, SourceLayer, Transform, compile_scene, execute_cpu,
};
use fm_frame::{
    ChromaLocation, ClockDomainId, ColorMetadata, ColorPrimaries, CpuVideoFrame, CpuVideoPayload,
    CpuVideoPlane, MatrixCoefficients, MediaTimestamp, MediaTiming, NormalizedDuration,
    NormalizedTimestamp, OriginalTimestamp, PixelFormat, SequenceNumber, SignalRange, TimeBase,
    TransferFunction, VideoDimensions, VideoFrameMetadata,
};
use fm_gpu::{NativeBackend, NativeContext, NativeTexture, TextureFormat};
use fm_grade::{BUILTIN_PRESETS, Grade};
use fm_video::ImageFrame;
use half::f16;

/// Per-channel maximum premultiplied colour error, in 8-bit codes.
const MAX_ERROR: u8 = 2;
/// Per-channel maximum premultiplied colour error of a keyed layer (spill
/// follows key alpha), in 8-bit codes.
const KEY_MAX_ERROR: u8 = 4;
/// Per-channel maximum premultiplied colour error with grades, in 8-bit codes.
const GRADED_MAX_ERROR: u8 = 4;
/// Per-channel mean premultiplied colour error, in 8-bit codes.
const MEAN_ERROR: f64 = 0.25;
/// Mean alpha error, in 8-bit codes.
const MEAN_ALPHA_ERROR: f64 = 1.0;

/// Maximum colour and alpha errors for one case, in 8-bit codes.
#[derive(Clone, Copy)]
struct Budget {
    colour: u8,
    alpha: u8,
}

/// Unkeyed layers: colour within [`MAX_ERROR`], alpha exact.
const EFFECTS: Budget = Budget {
    colour: MAX_ERROR,
    alpha: 0,
};

/// A key whose ramp spans `softness` codes: two decoded codes of error
/// move key alpha by up to `2 * ceil(255 / softness)`, plus rounding.
fn key_budget(softness: u8) -> Budget {
    let step = if softness == 0 {
        u8::MAX
    } else {
        u8::MAX.div_ceil(softness)
    };
    Budget {
        colour: KEY_MAX_ERROR,
        alpha: step.saturating_mul(2).saturating_add(1),
    }
}

const WIDTH: u32 = 32;
const HEIGHT: u32 = 24;
const SOURCE: SourceId = SourceId::new(1);

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

fn srgb_frame(image: &ImageFrame) -> CpuVideoFrame {
    let payload = CpuVideoPayload::new(
        PixelFormat::Rgba8,
        VideoDimensions::new(image.width(), image.height()).unwrap(),
        vec![CpuVideoPlane::new(image.width() as usize * 4, image.pixels().to_vec()).unwrap()],
    )
    .unwrap();
    CpuVideoFrame::new(timing(), payload)
        .with_metadata(VideoFrameMetadata::new(
            ColorMetadata {
                primaries: ColorPrimaries::Bt709,
                transfer: TransferFunction::Srgb,
                matrix: MatrixCoefficients::Identity,
                range: SignalRange::Full,
                chroma_location: ChromaLocation::Center,
            },
            Some(AlphaMode::Straight),
        ))
        .unwrap()
}

/// A deterministic straight-alpha fixture: a noisy green screen down the left
/// and top edges, a full-range pseudo-random foreground with greenish spill
/// tones, and translucent (never fully transparent) pixels sprinkled in.
#[allow(clippy::cast_possible_truncation)]
fn fixture() -> ImageFrame {
    let mut pixels = Vec::new();
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let hash = (x * 7_919 + y * 104_729 + x * y * 31) ^ (x * 13 + y * 17);
            let byte = |shift: u32| (hash >> shift) as u8;
            let rgb = if x < 9 || y < 6 {
                [10 + byte(0) % 30, 170 + byte(3) % 70, 20 + byte(6) % 45]
            } else if (x + y) % 5 == 0 {
                // Spill-tinted foreground: green just above red and blue.
                let base = byte(2) % 160;
                [base, base + 40 + byte(5) % 50, base / 2]
            } else {
                [byte(1), byte(4), byte(9)]
            };
            let alpha = if (x * 3 + y) % 11 == 0 {
                64 + byte(7) % 180
            } else {
                255
            };
            pixels.extend_from_slice(&[rgb[0], rgb[1], rgb[2], alpha]);
        }
    }
    ImageFrame::new(WIDTH, HEIGHT, WIDTH as usize * 4, pixels).unwrap()
}

/// Straightens a premultiplied working pixel and encodes it as sRGB 8-bit
/// BT.709 with the SDR output transform's matrix.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn straight_display(pixel: [f32; 4]) -> ([u8; 3], u8) {
    let [r, g, b, a] = pixel;
    let alpha = (a.clamp(0.0, 1.0) * 255.0).round() as u8;
    if a <= 0.0 {
        return ([0; 3], alpha);
    }
    let (r, g, b) = (r / a, g / a, b / a);
    let rec709 = [
        1.660_491 * r - 0.587_641 * g - 0.072_850 * b,
        -0.124_550 * r + 1.132_9 * g - 0.008_349 * b,
        -0.018_151 * r - 0.100_579 * g + 1.118_73 * b,
    ];
    let encode = |linear: f32| (srgb_from_linear(linear.clamp(0.0, 1.0)) * 255.0).round() as u8;
    (rec709.map(encode), alpha)
}

fn multiply_u8(left: u8, right: u8) -> u8 {
    u8::try_from((u16::from(left) * u16::from(right) + 127) / 255).unwrap()
}

#[derive(Default)]
struct ErrorStats {
    max: [u8; 3],
    max_alpha: u8,
    sum: [u64; 3],
    alpha_sum: u64,
    samples: u64,
    /// Largest colour error with its pixel index, native straight pixel, and
    /// CPU premultiplied pixel.
    worst: (u8, usize, [u8; 4], [u8; 4]),
}

impl ErrorStats {
    /// `actual` is the native straight pixel, `expected` the CPU's
    /// premultiplied pixel.
    fn add(&mut self, index: usize, actual: ([u8; 3], u8), expected: [u8; 4]) {
        let (rgb, alpha) = actual;
        self.max_alpha = self.max_alpha.max(alpha.abs_diff(expected[3]));
        self.alpha_sum += u64::from(alpha.abs_diff(expected[3]));
        for channel in 0..3 {
            let error = multiply_u8(rgb[channel], expected[3]).abs_diff(expected[channel]);
            self.max[channel] = self.max[channel].max(error);
            self.sum[channel] += u64::from(error);
            if error > self.worst.0 {
                self.worst = (error, index, [rgb[0], rgb[1], rgb[2], alpha], expected);
            }
        }
        self.samples += 1;
    }

    #[allow(clippy::cast_precision_loss)]
    fn assert_within(&self, case: &str, budget: Budget) -> (u8, u8) {
        let mean = self.sum.map(|sum| sum as f64 / self.samples as f64);
        let mean_alpha = self.alpha_sum as f64 / self.samples as f64;
        eprintln!(
            "{case}: max {:?}/255 (alpha {}), mean [{:.4}, {:.4}, {:.4}]/255 (alpha {:.4}) \
             over {} pixels; worst at {} native straight {:?} CPU premultiplied {:?}",
            self.max,
            self.max_alpha,
            mean[0],
            mean[1],
            mean[2],
            mean_alpha,
            self.samples,
            self.worst.1,
            self.worst.2,
            self.worst.3,
        );
        assert!(
            self.max_alpha <= budget.alpha,
            "{case}: alpha error {} > {}",
            self.max_alpha,
            budget.alpha
        );
        assert!(
            mean_alpha <= MEAN_ALPHA_ERROR,
            "{case}: mean alpha error {mean_alpha} > {MEAN_ALPHA_ERROR}"
        );
        for (channel, (max, mean)) in self.max.into_iter().zip(mean).enumerate() {
            assert!(
                max <= budget.colour,
                "{case}: channel {channel} max error {max} > {}",
                budget.colour
            );
            assert!(
                mean <= MEAN_ERROR,
                "{case}: channel {channel} mean error {mean} > {MEAN_ERROR}"
            );
        }
        (self.max.into_iter().max().unwrap_or(0), self.max_alpha)
    }
}

struct Harness {
    context: NativeContext,
    renderer: NativeCompositionRenderer,
    image: ImageFrame,
    source: NativeWorkingFrame,
}

impl Harness {
    fn new(test: &str) -> Option<Self> {
        let context = native_context(test)?;
        let image = fixture();
        block_on(async {
            let normalizer = NativeImportNormalizer::new(&context).await.unwrap();
            let source = normalizer
                .normalize(&context, &srgb_frame(&image))
                .await
                .unwrap();
            let renderer = NativeCompositionRenderer::new(&context).await.unwrap();
            Some(Self {
                context,
                renderer,
                image,
                source,
            })
        })
    }

    async fn raw(&self, texture: &NativeTexture) -> Vec<[f32; 4]> {
        let readback = self.context.readback(texture).await.unwrap();
        assert_eq!(readback.format, TextureFormat::Rgba16Float);
        readback
            .bytes
            .as_chunks::<8>()
            .0
            .iter()
            .map(|pixel| {
                core::array::from_fn(|component| {
                    f16::from_le_bytes([pixel[component * 2], pixel[component * 2 + 1]]).to_f32()
                })
            })
            .collect()
    }

    /// Renders `layer` alone over a transparent `width x height` canvas on
    /// both paths and checks parity; returns the largest colour and alpha
    /// errors.
    fn check(
        &self,
        case: &str,
        width: u32,
        height: u32,
        layer: SourceLayer,
        budget: Budget,
    ) -> (u8, u8) {
        let mut scene = Scene::new(width, height, Rgba8::new(0, 0, 0, 0)).unwrap();
        scene.push_layer(layer);
        let plan = compile_scene(&scene, OutputTarget::Program).unwrap().0;
        let expected = execute_cpu(&plan, &[CpuSourceFrame::new(SOURCE, &self.image)]).unwrap();
        block_on(async {
            let composed = self
                .renderer
                .render(
                    &self.context,
                    &plan,
                    &[NativeSourceFrame::new(SOURCE, self.source.texture())],
                )
                .await
                .unwrap();
            assert_eq!((composed.width(), composed.height()), (width, height));
            let mut stats = ErrorStats::default();
            for (index, (actual, expected)) in self
                .raw(&composed)
                .await
                .into_iter()
                .zip(expected.pixels().as_chunks::<4>().0)
                .enumerate()
            {
                stats.add(index, straight_display(actual), *expected);
            }
            stats.assert_within(case, budget)
        })
    }
}

/// The largest colour and alpha errors seen across a test's cases.
#[derive(Default)]
struct Worst(u8, u8);

impl Worst {
    fn add(&mut self, (colour, alpha): (u8, u8)) {
        self.0 = self.0.max(colour);
        self.1 = self.1.max(alpha);
    }

    fn report(&self, test: &str) {
        eprintln!(
            "{test}: worst colour error {}/255, alpha error {}/255",
            self.0, self.1
        );
    }
}

fn full_layer() -> SourceLayer {
    SourceLayer::new(
        SOURCE,
        0,
        Transform::new(0, 0, WIDTH, HEIGHT, Rotation::Deg0),
    )
}

const GREEN_SOFTNESS: u8 = 70;

fn green_screen(spill: u8) -> ChromaKey {
    ChromaKey::new(Rgba8::new(25, 205, 42, 255), 30, GREEN_SOFTNESS, spill)
}

#[test]
fn chroma_keys_with_spill_match_the_cpu_reference() {
    let Some(harness) = Harness::new("chroma_keys_with_spill_match_the_cpu_reference") else {
        return;
    };
    let mut worst = Worst::default();
    for (case, key) in [
        ("chroma, no spill", green_screen(0)),
        ("chroma, spill 200", green_screen(200)),
        ("chroma, full spill", green_screen(255)),
        (
            "chroma, hard edge",
            ChromaKey::new(Rgba8::new(25, 205, 42, 255), 60, 0, 128),
        ),
        (
            "chroma, blue screen",
            ChromaKey::new(Rgba8::new(30, 40, 220, 255), 90, 120, 180),
        ),
        (
            "chroma, red dominant",
            ChromaKey::new(Rgba8::new(210, 60, 40, 255), 120, 90, 255),
        ),
    ] {
        worst.add(harness.check(
            case,
            WIDTH,
            HEIGHT,
            full_layer().with_key(Key::Chroma(key)),
            key_budget(key.softness),
        ));
    }
    worst.report("chroma keys");
}

#[test]
fn luma_keys_with_high_bound_and_invert_match_the_cpu_reference() {
    let Some(harness) =
        Harness::new("luma_keys_with_high_bound_and_invert_match_the_cpu_reference")
    else {
        return;
    };
    let mut worst = Worst::default();
    for (case, key) in [
        ("luma low only", LumaKey::new(70, 50, false)),
        ("luma band", LumaKey::new(60, 40, false).with_high(190)),
        (
            "luma band inverted",
            LumaKey::new(60, 40, true).with_high(190),
        ),
        ("luma hard band", LumaKey::new(90, 0, false).with_high(150)),
        ("luma inverted low only", LumaKey::new(120, 25, true)),
    ] {
        worst.add(harness.check(
            case,
            WIDTH,
            HEIGHT,
            full_layer().with_key(Key::Luma(key)),
            key_budget(key.softness),
        ));
    }
    worst.report("luma keys");
}

#[test]
fn every_reference_effect_matches_the_cpu_reference() {
    let Some(harness) = Harness::new("every_reference_effect_matches_the_cpu_reference") else {
        return;
    };
    let mut worst = Worst::default();
    for (name, parameters) in [
        ("levels", vec![30, 220]),
        ("levels", vec![0, 1]),
        ("brightness_contrast", vec![-25, 40]),
        ("brightness_contrast", vec![100, -100]),
        ("saturation", vec![-100]),
        ("saturation", vec![-60]),
        ("saturation", vec![80]),
        ("sharpen", vec![750]),
        ("sharpen", vec![1000]),
        ("flip", vec![1, 0]),
        ("flip", vec![0, 1]),
        ("flip", vec![1, 1]),
        ("deinterlace", vec![0]),
        ("deinterlace", vec![1]),
        ("deinterlace", vec![2]),
    ] {
        let case = format!("{name} {parameters:?}");
        let layer = full_layer().with_effect(Effect::new(name, parameters));
        worst.add(harness.check(&case, WIDTH, HEIGHT, layer, EFFECTS));
    }
    // A chain, with a passthrough no-op in the middle.
    let chain = full_layer()
        .with_effect(Effect::new("levels", vec![16, 235]))
        .with_effect(Effect::passthrough())
        .with_effect(Effect::new("sharpen", vec![600]))
        .with_effect(Effect::new("saturation", vec![35]))
        .with_effect(Effect::new("brightness_contrast", vec![10, 20]));
    worst.add(harness.check("effect chain", WIDTH, HEIGHT, chain, EFFECTS));
    worst.report("effects");
}

#[test]
fn cropped_masked_keyed_and_flipped_layers_match_the_cpu_reference() {
    let Some(harness) =
        Harness::new("cropped_masked_keyed_and_flipped_layers_match_the_cpu_reference")
    else {
        return;
    };
    let effects = || {
        [
            Effect::new("saturation", vec![40]),
            Effect::new("sharpen", vec![500]),
            Effect::passthrough(),
            Effect::new("flip", vec![1, 1]),
        ]
    };
    let mut worst = Worst::default();
    // Cropped, masked, keyed, filtered, flipped, placed 1:1 off the origin.
    let mut placed = SourceLayer::new(SOURCE, 0, Transform::new(5, 4, 26, 20, Rotation::Deg0))
        .with_crop(CropRect::new(3, 2, 26, 20))
        .with_mask(RectMask::new(2, 3, 18, 12))
        .with_key(Key::Chroma(green_screen(160)));
    for effect in effects() {
        placed = placed.with_effect(effect);
    }
    worst.add(harness.check(
        "crop+mask+key+effects",
        40,
        30,
        placed,
        key_budget(GREEN_SOFTNESS),
    ));

    // An inverted mask, a luma key, and a scaled quarter-turned placement
    // with a border and partial opacity.
    let luma = LumaKey::new(40, 30, false).with_high(220);
    let mut turned = SourceLayer::new(SOURCE, 0, Transform::new(3, 2, 30, 22, Rotation::Deg90))
        .with_crop(CropRect::new(4, 1, 25, 21))
        .with_mask(RectMask::new(6, 5, 10, 9).inverted(true))
        .with_key(Key::Luma(luma))
        .with_inset_border(1)
        .with_opacity(200);
    for effect in effects() {
        turned = turned.with_effect(effect);
    }
    worst.add(harness.check(
        "inverted mask, luma key, rotated, scaled, opacity",
        40,
        36,
        turned,
        key_budget(luma.softness),
    ));

    // A mask with effects but no key also moves into the preparation pass
    // (a flip must mirror the mask with the pixels, as on the CPU).
    let masked_flip = full_layer()
        .with_crop(CropRect::new(1, 1, 30, 22))
        .with_mask(RectMask::new(0, 0, 12, 7))
        .with_effect(Effect::new("flip", vec![1, 0]));
    worst.add(harness.check("mask+flip, no key", WIDTH, HEIGHT, masked_flip, EFFECTS));
    worst.report("combinations");
}

#[test]
fn graded_keyed_layers_match_the_cpu_reference() {
    let Some(harness) = Harness::new("graded_keyed_layers_match_the_cpu_reference") else {
        return;
    };
    let warm = Arc::new(CompiledGrade::new(&BUILTIN_PRESETS[1].grade, None).unwrap());
    let exposure = Arc::new(
        CompiledGrade::new(
            &Grade {
                exposure_millistops: 500,
                saturation_permille: 800,
                ..Grade::IDENTITY
            },
            None,
        )
        .unwrap(),
    );
    let graded = |softness| Budget {
        colour: GRADED_MAX_ERROR,
        ..key_budget(softness)
    };
    let mut worst = Worst::default();
    let keyed = full_layer()
        .with_crop(CropRect::new(2, 2, 28, 20))
        .with_key(Key::Chroma(green_screen(200)))
        .with_effect(Effect::new("levels", vec![10, 240]))
        .with_grade(warm.clone());
    worst.add(harness.check(
        "chroma key, levels, source grade",
        WIDTH,
        HEIGHT,
        keyed,
        graded(GREEN_SOFTNESS),
    ));
    let luma = LumaKey::new(50, 30, true).with_high(200);
    let banded = full_layer()
        .with_key(Key::Luma(luma))
        .with_mask(RectMask::new(4, 3, 20, 16))
        .with_grade(exposure)
        .with_layer_grade(warm);
    worst.add(harness.check(
        "luma key, mask, source and layer grades",
        WIDTH,
        HEIGHT,
        banded,
        graded(luma.softness),
    ));
    worst.report("graded keyed layers");
}

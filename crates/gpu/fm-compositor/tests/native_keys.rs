//! SW-008 native keying parity: key/fill (linear key from a separate key
//! source's luma or alpha, gain/clip and invert, straight and premultiplied
//! fill), alpha-channel sources, and key refinements (clip, choke/spread,
//! edge softness, green/blue despill, garbage mask) against `execute_cpu`.
//!
//! Runs on any available wgpu adapter (including software Vulkan such as
//! lavapipe). Without an adapter the tests skip, unless
//! `FM_REQUIRE_NATIVE_MEDIA=1` makes that a failure.
//!
//! Scenes compose over a transparent background; each native pixel is
//! straightened and encoded to sRGB 8-bit BT.709, its alpha compared with the
//! CPU alpha and its colour, premultiplied by the CPU alpha in 8 bits, with
//! the CPU's premultiplied colour (as in `native_layer_effects.rs`).
//!
//! Error budget. The native passes run the CPU's exact integer formulas on
//! codes decoded from the binary16 working textures, so the only error source
//! is that decode (up to 2 codes in a dark channel next to a bright one; a
//! gray key signal decodes exactly). A decoded code that is off by one moves
//! a key ramp by `255 / softness`, and a clip range of `white - black` codes
//! multiplies that by `255 / (white - black)`; choke/spread (window min/max)
//! and edge softness (window mean) never enlarge an error. Premultiplied CPU
//! input adds the CPU's own premultiplied rounding. Every case is also held
//! to a small mean error, so the maxima stay rare edge pixels.

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
    AlphaMode, Backing, ChromaKey, CpuExecutionError, CpuSourceFrame, CropRect, Despill, Effect,
    Key, KeyChannel, KeyFill, KeyRefinement, LightWrap, LumaKey, NativeCompositionError,
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
use fm_video::ImageFrame;
use half::f16;

/// Per-channel maximum premultiplied colour error, in 8-bit codes.
const MAX_ERROR: u8 = 4;
/// Per-channel mean premultiplied colour error, in 8-bit codes.
const MEAN_ERROR: f64 = 0.3;
/// Mean alpha error, in 8-bit codes.
const MEAN_ALPHA_ERROR: f64 = 1.0;

const WIDTH: u32 = 32;
const HEIGHT: u32 = 24;
const FILL: SourceId = SourceId::new(1);
const KEY: SourceId = SourceId::new(2);
const FILL_PREMULTIPLIED: SourceId = SourceId::new(3);
const KEY_ALPHA: SourceId = SourceId::new(4);
const CLEAR_FILL: SourceId = SourceId::new(5);
const SMALL_KEY: SourceId = SourceId::new(6);
const OPAQUE_FILL: SourceId = SourceId::new(7);

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

fn image(width: u32, height: u32, pixel: impl Fn(u32, u32) -> [u8; 4]) -> ImageFrame {
    let mut pixels = Vec::new();
    for y in 0..height {
        for x in 0..width {
            pixels.extend_from_slice(&pixel(x, y));
        }
    }
    ImageFrame::new(width, height, width as usize * 4, pixels).unwrap()
}

fn hash(x: u32, y: u32) -> u32 {
    (x * 7_919 + y * 104_729 + x * y * 31) ^ (x * 13 + y * 17)
}

/// A straight fill: a noisy green screen down the left and top edges, a
/// pseudo-random foreground with greenish spill tones, and translucent
/// (never fully transparent) pixels sprinkled in.
#[allow(clippy::cast_possible_truncation)]
fn fill(x: u32, y: u32) -> [u8; 4] {
    let hash = hash(x, y);
    let byte = |shift: u32| (hash >> shift) as u8;
    let rgb = if x < 9 || y < 6 {
        [10 + byte(0) % 30, 170 + byte(3) % 70, 20 + byte(6) % 45]
    } else if (x + y).is_multiple_of(5) {
        let base = byte(2) % 160;
        [base, base + 40 + byte(5) % 50, base / 2]
    } else if (x + 2 * y).is_multiple_of(7) {
        // Blue-spill tones for the blue despill case.
        let base = byte(4) % 150;
        [base, base / 2 + 10, base + 50 + byte(1) % 50]
    } else {
        [byte(1), byte(4), byte(9)]
    };
    let alpha = if (x * 3 + y).is_multiple_of(11) {
        64 + byte(7) % 180
    } else {
        255
    };
    [rgb[0], rgb[1], rgb[2], alpha]
}

/// An opaque key signal: a soft gray ellipse matte with a few coloured
/// pixels (a key source is not always clean gray).
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn key(x: u32, y: u32) -> [u8; 4] {
    let dx = (f64::from(x) - 15.5) / 12.0;
    let dy = (f64::from(y) - 11.5) / 9.0;
    let distance = (dx * dx + dy * dy).sqrt();
    let level = ((1.4 - distance) * 255.0).clamp(0.0, 255.0) as u8;
    if (x + y * 3).is_multiple_of(13) {
        let byte = hash(x, y) as u8;
        [level, level / 2 + byte % 60, byte, 255]
    } else {
        [level, level, level, 255]
    }
}

/// A key source whose alpha (including fully transparent and opaque pixels)
/// is the matte; its colour is irrelevant noise.
#[allow(clippy::cast_possible_truncation)]
fn key_alpha(x: u32, y: u32) -> [u8; 4] {
    let byte = hash(x, y) as u8;
    let alpha = match (x + y) % 9 {
        0 => 0,
        1 => 255,
        _ => ((x * 11 + y * 7) % 256) as u8,
    };
    [byte, byte / 3, 255 - byte, alpha]
}

fn premultiply(pixel: [u8; 4]) -> [u8; 4] {
    let scale = |channel: u8| multiply_u8(channel, pixel[3]);
    [scale(pixel[0]), scale(pixel[1]), scale(pixel[2]), pixel[3]]
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

/// Maximum colour and alpha errors for one case, in 8-bit codes.
#[derive(Clone, Copy, Debug)]
struct Budget {
    colour: u8,
    alpha: u8,
}

/// A ramp over `softness` codes, steepened by a clip of `range` codes:
/// two decoded codes of error move alpha by
/// `2 * ceil(255 / softness) * ceil(255 / range)`, plus rounding.
fn budget(softness: u8, range: u8) -> Budget {
    let step = |span: u8| {
        if span == 0 {
            255
        } else {
            u32::from(u8::MAX.div_ceil(span))
        }
    };
    let alpha = (2 * step(softness) * step(range) + 1).min(255);
    Budget {
        colour: MAX_ERROR,
        alpha: u8::try_from(alpha).unwrap(),
    }
}

/// A key/fill matte from a gray key: its decode is exact, so only the
/// matte's own rounding remains.
const EXACT_MATTE: Budget = Budget {
    colour: MAX_ERROR,
    alpha: 1,
};

#[derive(Default)]
struct ErrorStats {
    max: [u8; 3],
    max_alpha: u8,
    sum: [u64; 3],
    alpha_sum: u64,
    samples: u64,
    worst: (u8, usize, [u8; 4], [u8; 4]),
}

impl ErrorStats {
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

struct Source {
    id: SourceId,
    image: ImageFrame,
    native: NativeWorkingFrame,
}

struct Harness {
    context: NativeContext,
    renderer: NativeCompositionRenderer,
    sources: Vec<Source>,
}

impl Harness {
    fn new(test: &str) -> Option<Self> {
        let context = native_context(test)?;
        let straight = image(WIDTH, HEIGHT, fill);
        let premultiplied = image(WIDTH, HEIGHT, |x, y| premultiply(fill(x, y)));
        // Native import normalizes straight sRGB into the premultiplied
        // working space, so a premultiplied CPU layer is the same picture
        // imported from its straight form.
        let inputs = [
            (FILL, straight.clone(), None),
            (KEY, image(WIDTH, HEIGHT, key), None),
            (FILL_PREMULTIPLIED, premultiplied, Some(straight)),
            (KEY_ALPHA, image(WIDTH, HEIGHT, key_alpha), None),
            (
                CLEAR_FILL,
                image(WIDTH, HEIGHT, |x, y| {
                    let [r, g, b, _] = fill(x, y);
                    [r, g, b, 0]
                }),
                None,
            ),
            (SMALL_KEY, image(WIDTH - 1, HEIGHT, key), None),
            (
                OPAQUE_FILL,
                image(WIDTH, HEIGHT, |x, y| {
                    let [r, g, b, _] = fill(x, y);
                    [r, g, b, 255]
                }),
                None,
            ),
        ];
        block_on(async {
            let normalizer = NativeImportNormalizer::new(&context).await.unwrap();
            let mut sources = Vec::new();
            for (id, image, straight) in inputs {
                let native = normalizer
                    .normalize(&context, &srgb_frame(straight.as_ref().unwrap_or(&image)))
                    .await
                    .unwrap();
                sources.push(Source { id, image, native });
            }
            let renderer = NativeCompositionRenderer::new(&context).await.unwrap();
            Some(Self {
                context,
                renderer,
                sources,
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

    fn plan(layer: SourceLayer, width: u32, height: u32) -> fm_compositor::CompositionPlan {
        let mut scene = Scene::new(width, height, Rgba8::new(0, 0, 0, 0)).unwrap();
        scene.push_layer(layer);
        compile_scene(&scene, OutputTarget::Program).unwrap().0
    }

    fn cpu_sources(&self, skip: Option<SourceId>) -> Vec<CpuSourceFrame<'_>> {
        self.sources
            .iter()
            .filter(|source| Some(source.id) != skip)
            .map(|source| CpuSourceFrame::new(source.id, &source.image))
            .collect()
    }

    fn native_sources(&self, skip: Option<SourceId>) -> Vec<NativeSourceFrame<'_>> {
        self.sources
            .iter()
            .filter(|source| Some(source.id) != skip)
            .map(|source| NativeSourceFrame::new(source.id, source.native.texture()))
            .collect()
    }

    /// Renders `layer` alone over a transparent canvas on both paths and
    /// checks parity; returns the largest colour and alpha errors.
    fn check(&self, case: &str, layer: SourceLayer, budget: Budget) -> (u8, u8) {
        self.check_sized(case, WIDTH, HEIGHT, layer, budget)
    }

    fn check_sized(
        &self,
        case: &str,
        width: u32,
        height: u32,
        layer: SourceLayer,
        budget: Budget,
    ) -> (u8, u8) {
        self.check_plan(case, &Self::plan(layer, width, height), budget)
    }

    /// Renders `plan` on both paths and checks parity.
    fn check_plan(
        &self,
        case: &str,
        plan: &fm_compositor::CompositionPlan,
        budget: Budget,
    ) -> (u8, u8) {
        let (width, height) = (plan.width(), plan.height());
        let expected = execute_cpu(plan, &self.cpu_sources(None)).unwrap();
        block_on(async {
            let composed = self
                .renderer
                .render(&self.context, plan, &self.native_sources(None))
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

fn full_layer(source: SourceId) -> SourceLayer {
    SourceLayer::new(
        source,
        0,
        Transform::new(0, 0, WIDTH, HEIGHT, Rotation::Deg0),
    )
}

const GREEN_SOFTNESS: u8 = 70;

fn green_screen(spill: u8) -> ChromaKey {
    ChromaKey::new(Rgba8::new(25, 205, 42, 255), 30, GREEN_SOFTNESS, spill)
}

#[test]
fn key_fill_matches_the_cpu_reference() {
    let Some(harness) = Harness::new("key_fill_matches_the_cpu_reference") else {
        return;
    };
    let mut worst = Worst::default();
    // Luma-channel key fills read a mostly gray key whose few coloured
    // pixels can decode a code off, so they get the ramp budget.
    for (case, matte, fill) in [
        (
            "linear key, straight fill",
            LumaKey::new(0, 255, false),
            FILL,
        ),
        (
            "linear key, premultiplied fill",
            LumaKey::new(0, 255, false),
            FILL_PREMULTIPLIED,
        ),
        ("gain/clip", LumaKey::new(40, 120, false), FILL),
        (
            "inverted with high bound",
            LumaKey::new(20, 60, true).with_high(230),
            FILL,
        ),
        ("hard clip", LumaKey::new(128, 0, false), FILL_PREMULTIPLIED),
    ] {
        let alpha_mode = if fill == FILL_PREMULTIPLIED {
            AlphaMode::Premultiplied
        } else {
            AlphaMode::Straight
        };
        worst.add(
            harness.check(
                case,
                full_layer(fill)
                    .with_alpha_mode(alpha_mode)
                    .with_key_fill(KeyFill::new(KEY, matte)),
                budget(matte.softness, u8::MAX),
            ),
        );
    }
    // Alpha-channel key fills read stored alpha, which round-trips exactly.
    for (case, matte) in [
        ("alpha channel, linear", LumaKey::new(0, 255, false)),
        ("alpha channel, gain and invert", LumaKey::new(60, 90, true)),
    ] {
        worst.add(
            harness.check(
                case,
                full_layer(FILL)
                    .with_key_fill(KeyFill::new(KEY_ALPHA, matte).with_channel(KeyChannel::Alpha)),
                EXACT_MATTE,
            ),
        );
    }
    // The fill's own alpha used as its key (a source with alpha keyed by
    // itself): key source == fill source.
    worst.add(harness.check(
        "own alpha channel as key",
        full_layer(KEY_ALPHA).with_key_fill(
            KeyFill::new(KEY_ALPHA, LumaKey::new(30, 60, false)).with_channel(KeyChannel::Alpha),
        ),
        EXACT_MATTE,
    ));
    worst.report("key/fill");
}

#[test]
fn sources_with_alpha_composite_like_the_cpu_reference() {
    let Some(harness) = Harness::new("sources_with_alpha_composite_like_the_cpu_reference") else {
        return;
    };
    let mut worst = Worst::default();
    // Unkeyed straight and premultiplied sources with an alpha channel.
    worst.add(harness.check("straight alpha source", full_layer(KEY_ALPHA), EXACT_MATTE));
    worst.add(harness.check(
        "premultiplied alpha source",
        full_layer(FILL_PREMULTIPLIED).with_alpha_mode(AlphaMode::Premultiplied),
        EXACT_MATTE,
    ));
    worst.report("alpha sources");
}

#[test]
fn key_refinements_match_the_cpu_reference() {
    let Some(harness) = Harness::new("key_refinements_match_the_cpu_reference") else {
        return;
    };
    let mut worst = Worst::default();
    let chroma = Key::Chroma(green_screen(0));
    let spill = Key::Chroma(green_screen(180));
    for (case, key, refinement, range) in [
        ("identity refinement", chroma, KeyRefinement::new(), u8::MAX),
        ("clip", chroma, KeyRefinement::new().with_clip(40, 220), 180),
        (
            "choke 2",
            chroma,
            KeyRefinement::new().with_choke(2),
            u8::MAX,
        ),
        (
            "spread 3",
            chroma,
            KeyRefinement::new().with_choke(-3),
            u8::MAX,
        ),
        (
            "choke 8",
            chroma,
            KeyRefinement::new().with_choke(8),
            u8::MAX,
        ),
        (
            "edge softness 1",
            chroma,
            KeyRefinement::new().with_edge_softness(1),
            u8::MAX,
        ),
        (
            "edge softness 8",
            chroma,
            KeyRefinement::new().with_edge_softness(8),
            u8::MAX,
        ),
        (
            "green despill with spill",
            spill,
            KeyRefinement::new().with_despill(Despill::new(Backing::Green, 255)),
            u8::MAX,
        ),
        (
            "blue despill, luma key",
            Key::Luma(LumaKey::new(30, 60, false)),
            KeyRefinement::new().with_despill(Despill::new(Backing::Blue, 160)),
            u8::MAX,
        ),
        (
            "clip, choke, softness, despill",
            spill,
            KeyRefinement::new()
                .with_clip(20, 235)
                .with_choke(1)
                .with_edge_softness(2)
                .with_despill(Despill::new(Backing::Green, 200)),
            215,
        ),
    ] {
        let softness = match key {
            Key::Chroma(chroma) => chroma.softness,
            Key::Luma(luma) => luma.softness,
        };
        worst.add(
            harness.check(
                case,
                full_layer(FILL)
                    .with_key(key)
                    .with_key_refinement(refinement),
                budget(softness, range),
            ),
        );
    }
    worst.report("refinements");
}

#[test]
fn refined_key_fill_with_crop_garbage_mask_and_effects_matches_the_cpu_reference() {
    let Some(harness) = Harness::new(
        "refined_key_fill_with_crop_garbage_mask_and_effects_matches_the_cpu_reference",
    ) else {
        return;
    };
    let mut worst = Worst::default();
    // Cropped key/fill (both inputs cropped alike), clip, spread, softness,
    // an inverted garbage mask, effects, and a scaled placement.
    let layer = SourceLayer::new(FILL, 0, Transform::new(3, 2, 30, 22, Rotation::Deg0))
        .with_crop(CropRect::new(2, 1, 27, 21))
        .with_key_fill(KeyFill::new(KEY, LumaKey::new(10, 200, false)))
        .with_key_refinement(
            KeyRefinement::new()
                .with_clip(15, 240)
                .with_choke(-2)
                .with_edge_softness(3)
                .with_despill(Despill::new(Backing::Green, 255)),
        )
        .with_mask(RectMask::new(4, 3, 9, 8).inverted(true))
        .with_effect(Effect::new("saturation", vec![30]))
        .with_opacity(220);
    worst.add(harness.check_sized(
        "cropped refined key/fill, garbage mask, effect",
        40,
        30,
        layer,
        budget(200, 225),
    ));
    // A refined chroma key with a garbage mask that keeps only a window.
    let layer = full_layer(FILL)
        .with_key(Key::Chroma(green_screen(120)))
        .with_key_refinement(KeyRefinement::new().with_choke(1).with_edge_softness(1))
        .with_mask(RectMask::new(5, 4, 20, 15));
    worst.add(harness.check(
        "refined chroma key, garbage mask",
        layer,
        budget(GREEN_SOFTNESS, u8::MAX),
    ));
    // Polygon garbage mattes, kept inside and outside, on a cropped and
    // scaled layer.
    let polygon = fm_compositor::GarbageMatte::new(
        &[
            fm_compositor::MattePoint::new(50, 80),
            fm_compositor::MattePoint::new(900, 150),
            fm_compositor::MattePoint::new(720, 600),
            fm_compositor::MattePoint::new(960, 940),
            fm_compositor::MattePoint::new(200, 870),
            fm_compositor::MattePoint::new(410, 430),
        ],
        false,
    )
    .unwrap();
    let matted = |matte| {
        SourceLayer::new(FILL, 0, Transform::new(1, 2, 30, 21, Rotation::Deg0))
            .with_crop(CropRect::new(3, 2, 25, 19))
            .with_key(Key::Chroma(green_screen(0)))
            .with_key_refinement(KeyRefinement::new().with_choke(1).with_garbage_matte(matte))
    };
    for (case, matte) in [
        ("polygon garbage matte", polygon),
        ("inverted polygon garbage matte", polygon.with_invert(true)),
        (
            "feathered polygon garbage matte",
            polygon.with_feather(4).unwrap(),
        ),
        (
            "feathered inverted polygon garbage matte",
            polygon.with_invert(true).with_feather(9).unwrap(),
        ),
    ] {
        worst.add(harness.check(case, matted(matte), budget(GREEN_SOFTNESS, u8::MAX)));
    }
    // The feather changes what the matte keeps, so the cases above compare
    // a real ramp.
    let render = |matte| {
        execute_cpu(
            &Harness::plan(matted(matte), WIDTH, HEIGHT),
            &harness.cpu_sources(None),
        )
        .unwrap()
    };
    assert_ne!(render(polygon), render(polygon.with_feather(4).unwrap()));
    worst.report("refined combinations");
}

#[test]
fn light_wrap_matches_the_cpu_reference() {
    let Some(harness) = Harness::new("light_wrap_matches_the_cpu_reference") else {
        return;
    };
    let mut worst = Worst::default();
    // An opaque background (the gray ellipse key signal) under a hard-keyed
    // opaque foreground: every output pixel is either background or the
    // (wrapped) foreground, so the composite itself adds no error.
    let hard = KeyFill::new(KEY_ALPHA, LumaKey::new(128, 0, false)).with_channel(KeyChannel::Alpha);
    for (case, wrap, transform, rotation, opacity, background_width) in [
        (
            "light wrap 255 x 1",
            LightWrap::new(255, 1),
            (0, 0, WIDTH, HEIGHT),
            0,
            255,
            WIDTH,
        ),
        (
            "light wrap 160 x 4",
            LightWrap::new(160, 4),
            (0, 0, WIDTH, HEIGHT),
            0,
            255,
            WIDTH,
        ),
        (
            "light wrap 255 x 8, scaled",
            LightWrap::new(255, 8),
            (3, -2, 26, 30),
            0,
            255,
            WIDTH,
        ),
        // A background covering only the left half (a nested scene's
        // transparent canvas elsewhere): the wrap weighs it by its alpha.
        (
            "light wrap 255 x 8, half transparent background",
            LightWrap::new(255, 8),
            (0, 0, WIDTH, HEIGHT),
            0,
            255,
            WIDTH / 2,
        ),
    ] {
        let mut scene = Scene::new(WIDTH, HEIGHT, Rgba8::new(0, 0, 0, 0)).unwrap();
        scene.push_layer(SourceLayer::new(
            KEY,
            0,
            Transform::new(0, 0, background_width, HEIGHT, Rotation::Deg0),
        ));
        let (x, y, width, height) = transform;
        scene.push_layer(
            SourceLayer::new(
                OPAQUE_FILL,
                1,
                Transform::new(x, y, width, height, Rotation::Deg0),
            )
            .with_rotation_millidegrees(rotation)
            .with_opacity(opacity)
            .with_key_fill(hard)
            .with_key_refinement(KeyRefinement::new().with_light_wrap(wrap)),
        );
        // A layer above the wrapped one still composites over it.
        scene.push_layer(
            SourceLayer::new(OPAQUE_FILL, 2, Transform::new(0, 0, 6, 5, Rotation::Deg0))
                .with_key_fill(hard),
        );
        let plan = compile_scene(&scene, OutputTarget::Program).unwrap().0;
        let budget = if rotation == 0 {
            EXACT_MATTE
        } else {
            // A tilted edge is bilinear, so its partial alpha composites in
            // linear light natively and on 8-bit codes on the CPU.
            Budget {
                colour: 48,
                alpha: 1,
            }
        };
        let (colour, alpha) = harness.check_plan(case, &plan, budget);
        if rotation == 0 {
            worst.add((colour, alpha));
        }
    }
    worst.report("light wrap");
}

#[test]
fn key_fill_edge_cases_have_defined_behaviour() {
    let Some(harness) = Harness::new("key_fill_edge_cases_have_defined_behaviour") else {
        return;
    };
    // A fully transparent fill stays fully transparent whatever its key.
    let transparent = full_layer(CLEAR_FILL)
        .with_key_fill(KeyFill::linear(KEY))
        .with_key_refinement(KeyRefinement::new().with_choke(-4).with_edge_softness(2));
    harness.check("fully transparent fill", transparent.clone(), EXACT_MATTE);
    let plan = Harness::plan(transparent, WIDTH, HEIGHT);
    block_on(async {
        let composed = harness
            .renderer
            .render(&harness.context, &plan, &harness.native_sources(None))
            .await
            .unwrap();
        assert!(
            harness
                .raw(&composed)
                .await
                .iter()
                .all(|pixel| pixel.iter().all(|&component| component == 0.0))
        );
    });

    // A missing key source is a typed error on both paths, before any GPU
    // work.
    let missing = Harness::plan(
        full_layer(FILL).with_key_fill(KeyFill::linear(KEY)),
        WIDTH,
        HEIGHT,
    );
    assert!(matches!(
        execute_cpu(&missing, &harness.cpu_sources(Some(KEY))),
        Err(CpuExecutionError::MissingSource(KEY))
    ));
    block_on(async {
        assert_eq!(
            harness
                .renderer
                .render(
                    &harness.context,
                    &missing,
                    &harness.native_sources(Some(KEY))
                )
                .await
                .err(),
            Some(NativeCompositionError::MissingSource(KEY))
        );
    });

    // A key source that is not the fill's size is refused on both paths.
    let mismatched = Harness::plan(
        full_layer(FILL).with_key_fill(KeyFill::linear(SMALL_KEY)),
        WIDTH,
        HEIGHT,
    );
    assert!(matches!(
        execute_cpu(&mismatched, &harness.cpu_sources(None)),
        Err(CpuExecutionError::KeyFillDimensionMismatch { layer: 0, .. })
    ));
    block_on(async {
        assert_eq!(
            harness
                .renderer
                .render(&harness.context, &mismatched, &harness.native_sources(None))
                .await
                .err(),
            Some(NativeCompositionError::KeyFillDimensionMismatch {
                layer: 0,
                fill_width: WIDTH,
                fill_height: HEIGHT,
                key_width: WIDTH - 1,
                key_height: HEIGHT,
            })
        );
    });
}

//! SW-019 native GPU grade parity against the `fm-grade` CPU reference.
//!
//! Runs on any available wgpu adapter (including software Vulkan such as
//! lavapipe). Without an adapter the tests skip, unless
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

use fm_color::{
    NativeImportNormalizer, NativeSdrOutputTransform, NativeWorkingFrame, srgb_from_linear,
};
use fm_compositor::{
    AlphaMode, CompiledGrade, CpuSourceFrame, DEFAULT_NATIVE_GRADE_LUT_EDGE,
    NativeCompositionRenderer, NativeGradeRenderer, NativeSourceFrame, OutputTarget, Rgba8,
    Rotation, Scene, SourceId, SourceLayer, Transform, compile_scene, execute_cpu,
};
use fm_frame::{
    ChromaLocation, ClockDomainId, ColorMetadata, ColorPrimaries, CpuVideoFrame, CpuVideoPayload,
    CpuVideoPlane, MatrixCoefficients, MediaTimestamp, MediaTiming, NormalizedDuration,
    NormalizedTimestamp, OriginalTimestamp, PixelFormat, SequenceNumber, SignalRange, TimeBase,
    TransferFunction, VideoDimensions, VideoFrameMetadata,
};
use fm_gpu::{NativeBackend, NativeContext, NativeTexture, TextureFormat};
use fm_grade::{
    BUILTIN_PRESETS, Contrast, CubeLut, Curve, CurvePoint, Grade, GradeLut, HueCurve, HueCurves,
    HuePoint, RgbCurves, Wheel, Wheels, WhiteBalance,
};
use fm_video::ImageFrame;
use half::f16;

/// Per-channel maximum absolute error budget, in 8-bit codes.
const MAX_ERROR: u8 = 2;
/// Interpolation bound for [`stress_cases`], in 8-bit codes.
const STRESS_MAX_ERROR: u8 = 4;
/// GPU against a CPU emulation of the same trilinear lookup, in 8-bit codes.
const EMULATION_MAX_ERROR: u8 = 1;
/// Per-channel mean absolute error budget, in 8-bit codes.
const MEAN_ERROR: f64 = 0.5;

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
        ClockDomainId::new(NonZeroU128::new(19).unwrap()),
        SequenceNumber::new(1),
    )
    .unwrap()
}

fn srgb_frame(image: &ImageFrame) -> CpuVideoFrame {
    let width = image.width();
    let height = image.height();
    let payload = CpuVideoPayload::new(
        PixelFormat::Rgba8,
        VideoDimensions::new(width, height).unwrap(),
        vec![CpuVideoPlane::new(width as usize * 4, image.pixels().to_vec()).unwrap()],
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

fn image(width: u32, height: u32, pixels: &[[u8; 4]]) -> ImageFrame {
    assert_eq!(pixels.len(), width as usize * height as usize);
    ImageFrame::new(width, height, width as usize * 4, pixels.concat()).unwrap()
}

/// 100% and 75% colour bars: white, yellow, cyan, green, magenta, red, blue,
/// black.
fn bars() -> ImageFrame {
    let mut pixels = Vec::new();
    for level in [255_u8, 191] {
        for [r, g, b] in [
            [1, 1, 1],
            [1, 1, 0],
            [0, 1, 1],
            [0, 1, 0],
            [1, 0, 1],
            [1, 0, 0],
            [0, 0, 1],
            [0, 0, 0],
        ] {
            pixels.push([r * level, g * level, b * level, 255]);
        }
    }
    image(8, 2, &pixels)
}

/// Full 0..=255 ramps: grey, red, green, blue, and a warm skin-tone ramp.
#[allow(clippy::cast_possible_truncation)]
fn ramps() -> ImageFrame {
    let mut pixels = Vec::new();
    for row in 0..5 {
        for code in 0..=255_u8 {
            pixels.push(match row {
                0 => [code, code, code, 255],
                1 => [code, 0, 0, 255],
                2 => [0, code, 0, 255],
                3 => [0, 0, code, 255],
                _ => [
                    code,
                    (u16::from(code) * 3 / 4) as u8,
                    (u16::from(code) / 2) as u8,
                    255,
                ],
            });
        }
    }
    image(256, 5, &pixels)
}

/// Every point of a 16³ RGB lattice (`code = 17 * index`), 64 x 64.
fn grid() -> ImageFrame {
    let mut pixels = Vec::new();
    for b in 0..16_u8 {
        for g in 0..16_u8 {
            for r in 0..16_u8 {
                pixels.push([r * 17, g * 17, b * 17, 255]);
            }
        }
    }
    image(64, 64, &pixels)
}

fn compiled(grade: &Grade, lut: Option<Arc<CubeLut>>) -> Arc<CompiledGrade> {
    Arc::new(CompiledGrade::new(grade, lut).unwrap())
}

fn film_lut() -> Arc<CubeLut> {
    let edge = 5_u8;
    let mut entries = Vec::new();
    for b in 0..edge {
        for g in 0..edge {
            for r in 0..edge {
                let value = |code: u8| f32::from(code) / f32::from(edge - 1);
                let (r, g, b) = (value(r), value(g), value(b));
                entries.push([
                    (r * 1.08 + 0.02 * g).min(1.0),
                    g * 0.97 + 0.03 * b,
                    b * b * 0.85 + 0.05,
                ]);
            }
        }
    }
    Arc::new(CubeLut::new_3d(usize::from(edge), entries, [0.0; 3], [1.0; 3]).unwrap())
}

fn full_grade() -> Grade {
    Grade {
        enabled: true,
        white_balance: WhiteBalance::new(300, -120),
        exposure_millistops: 400,
        contrast: Contrast::new(200, 4500),
        saturation_permille: 1200,
        wheels: Wheels {
            lift: Wheel::new(-100, 200, 50),
            gamma: Wheel::new(150, 0, -100),
            gain: Wheel::new(0, -250, 80),
            offset: Wheel::new(50, 50, -20),
        },
        curves: RgbCurves {
            master: Curve::new(vec![
                CurvePoint::new(0, 0),
                CurvePoint::new(3000, 2500),
                CurvePoint::new(7000, 7600),
                CurvePoint::new(10_000, 10_000),
            ]),
            red: Curve::new(vec![CurvePoint::new(0, 300), CurvePoint::new(10_000, 9800)]),
            green: Curve::IDENTITY,
            blue: Curve::new(vec![
                CurvePoint::new(0, 0),
                CurvePoint::new(5000, 4600),
                CurvePoint::new(10_000, 10_000),
            ]),
        },
        hue_curves: HueCurves {
            hue_vs_hue: HueCurve::new(vec![HuePoint::new(600, 100), HuePoint::new(2400, -80)]),
            hue_vs_saturation: HueCurve::new(vec![
                HuePoint::new(0, 200),
                HuePoint::new(1200, -300),
                HuePoint::new(2400, 0),
            ]),
            luma_vs_saturation: HueCurve::new(vec![
                HuePoint::new(0, -500),
                HuePoint::new(10_000, 0),
            ]),
        },
        lut: Some(GradeLut::new("asset://luts/film.cube", 600)),
    }
}

/// Every stage except hue curves at once, with a 3D `.cube` LUT.
fn primary_curves_lut_grade() -> Arc<CompiledGrade> {
    compiled(
        &Grade {
            hue_curves: HueCurves::IDENTITY,
            ..full_grade()
        },
        Some(film_lut()),
    )
}

/// Non-identity grades held to the 2/255 parity budget: the built-in presets,
/// exposure, every non-hue stage with a `.cube` LUT, and hue curves alone.
fn graded_cases() -> Vec<(String, Arc<CompiledGrade>)> {
    let mut cases = BUILTIN_PRESETS
        .iter()
        .filter(|preset| !preset.grade.is_identity())
        .map(|preset| {
            (
                format!("preset {}", preset.id),
                compiled(&preset.grade, None),
            )
        })
        .collect::<Vec<_>>();
    cases.push((
        "exposure -1.5 stops".to_owned(),
        compiled(
            &Grade {
                exposure_millistops: -1500,
                ..Grade::IDENTITY
            },
            None,
        ),
    ));
    cases.push((
        "primary, RGB curves and LUT".to_owned(),
        primary_curves_lut_grade(),
    ));
    cases.push((
        "hue curves".to_owned(),
        compiled(
            &Grade {
                hue_curves: full_grade().hue_curves,
                ..Grade::IDENTITY
            },
            None,
        ),
    ));
    cases
}

/// Every stage at once, including hue curves after strong primaries. HSV hue
/// curves vary quickly near the neutral axis, so no 65³ trilinear lattice
/// reproduces them within 2/255 everywhere (an exact CPU trilinear of the
/// same bake reaches 4/255 on a dense sweep). These are held to that
/// interpolation bound and, more tightly, to a CPU emulation of the shader.
fn stress_cases() -> Vec<(String, Arc<CompiledGrade>)> {
    vec![
        (
            "full grade with LUT".to_owned(),
            compiled(&full_grade(), Some(film_lut())),
        ),
        (
            "full grade, LUT missing".to_owned(),
            compiled(&full_grade(), None),
        ),
    ]
}

/// Absolute 8-bit error statistics for one fixture and grade.
#[derive(Default)]
struct ErrorStats {
    max: [u8; 3],
    sum: [u64; 3],
    samples: u64,
    /// Largest error with its GPU value, CPU value, and source pixel.
    worst: (u8, [u8; 3], [u8; 3], [u8; 4]),
}

impl ErrorStats {
    fn add(&mut self, actual: [u8; 3], expected: [u8; 3], source: [u8; 4]) {
        for channel in 0..3 {
            let error = actual[channel].abs_diff(expected[channel]);
            self.max[channel] = self.max[channel].max(error);
            self.sum[channel] += u64::from(error);
            if error > self.worst.0 {
                self.worst = (error, actual, expected, source);
            }
        }
        self.samples += 1;
    }

    #[allow(clippy::cast_precision_loss)]
    fn mean(&self) -> [f64; 3] {
        self.sum.map(|sum| sum as f64 / self.samples as f64)
    }

    fn assert_within_budget(&self, case: &str) {
        self.assert_within(case, MAX_ERROR, MEAN_ERROR);
    }

    fn assert_within(&self, case: &str, max_error: u8, mean_error: f64) {
        let mean = self.mean();
        eprintln!(
            "{case}: max {:?}/255, mean [{:.4}, {:.4}, {:.4}]/255 over {} pixels; worst GPU {:?} \
             CPU {:?} at source {:?}",
            self.max,
            mean[0],
            mean[1],
            mean[2],
            self.samples,
            self.worst.1,
            self.worst.2,
            self.worst.3
        );
        for (channel, (max, mean)) in self.max.into_iter().zip(mean).enumerate() {
            assert!(
                max <= max_error,
                "{case}: channel {channel} max error {max} > {max_error}"
            );
            assert!(
                mean <= mean_error,
                "{case}: channel {channel} mean error {mean} > {mean_error}"
            );
        }
    }
}

struct Harness {
    context: NativeContext,
    normalizer: NativeImportNormalizer,
    output: NativeSdrOutputTransform,
    grade: NativeGradeRenderer,
}

impl Harness {
    fn new(test: &str) -> Option<Self> {
        let context = native_context(test)?;
        block_on(async {
            let normalizer = NativeImportNormalizer::new(&context).await.unwrap();
            let output = NativeSdrOutputTransform::new(&context).await.unwrap();
            let grade = NativeGradeRenderer::new(&context).await.unwrap();
            Some(Self {
                context,
                normalizer,
                output,
                grade,
            })
        })
    }

    async fn import(&self, image: &ImageFrame) -> NativeWorkingFrame {
        self.normalizer
            .normalize(&self.context, &srgb_frame(image))
            .await
            .unwrap()
    }

    /// Opaque working texture to straight sRGB 8-bit through the canonical
    /// `fm-color` SDR output transform.
    async fn display(&self, working: &NativeTexture) -> Vec<[u8; 3]> {
        let target = self
            .context
            .create_rgba8_render_target(working.width(), working.height())
            .await
            .unwrap();
        self.output
            .transform(&self.context, working, &target)
            .await
            .unwrap();
        let readback = self.context.readback_rgba8(&target).await.unwrap();
        readback
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .map(|pixel| [pixel[0], pixel[1], pixel[2]])
            .collect()
    }

    async fn raw(&self, texture: &NativeTexture) -> Vec<u8> {
        let readback = self.context.readback(texture).await.unwrap();
        assert_eq!(readback.format, TextureFormat::Rgba16Float);
        readback.bytes
    }
}

fn rgb_pixels(image: &ImageFrame) -> Vec<[u8; 3]> {
    image
        .pixels()
        .as_chunks::<4>()
        .0
        .iter()
        .map(|pixel| [pixel[0], pixel[1], pixel[2]])
        .collect()
}

const fn opaque([r, g, b]: [u8; 3]) -> [u8; 4] {
    [r, g, b, 255]
}

fn fixtures() -> [(&'static str, ImageFrame); 3] {
    [("bars", bars()), ("ramps", ramps()), ("16^3 grid", grid())]
}

#[test]
fn identity_grades_skip_the_pass_and_stay_bit_exact() {
    let Some(harness) = Harness::new("identity_grades_skip_the_pass_and_stay_bit_exact") else {
        return;
    };
    block_on(async {
        let disabled = compiled(
            &Grade {
                enabled: false,
                ..full_grade()
            },
            None,
        );
        let neutral_lut_strength = compiled(
            &Grade {
                lut: Some(GradeLut::new("asset://luts/film.cube", 0)),
                ..Grade::IDENTITY
            },
            Some(film_lut()),
        );
        for (name, fixture) in fixtures() {
            let source = harness.import(&fixture).await;
            for grade in [
                Arc::new(CompiledGrade::identity()),
                disabled.clone(),
                neutral_lut_strength.clone(),
            ] {
                assert!(grade.is_identity());
                assert!(
                    harness
                        .grade
                        .prepare(&harness.context, &grade)
                        .await
                        .unwrap()
                        .is_none()
                );
                assert!(
                    harness
                        .grade
                        .grade_frame(&harness.context, &grade, source.texture())
                        .await
                        .unwrap()
                        .is_none(),
                    "{name}: identity grade must not render"
                );
            }

            // Identity-graded and ungraded layers compose bit-identically.
            let renderer = NativeCompositionRenderer::new(&harness.context)
                .await
                .unwrap();
            let layer = || {
                SourceLayer::new(
                    SourceId::new(1),
                    0,
                    Transform::new(0, 0, fixture.width(), fixture.height(), Rotation::Deg0),
                )
            };
            let compose = |layer: SourceLayer| {
                let mut scene =
                    Scene::new(fixture.width(), fixture.height(), Rgba8::new(0, 0, 0, 0)).unwrap();
                scene.push_layer(layer);
                compile_scene(&scene, OutputTarget::Program).unwrap().0
            };
            let sources = [NativeSourceFrame::new(SourceId::new(1), source.texture())];
            let plain = renderer
                .render(&harness.context, &compose(layer()), &sources)
                .await
                .unwrap();
            for grade in [Arc::new(CompiledGrade::identity()), disabled.clone()] {
                let graded = renderer
                    .render(
                        &harness.context,
                        &compose(layer().with_grade(grade)),
                        &sources,
                    )
                    .await
                    .unwrap();
                assert_eq!(
                    harness.raw(&graded).await,
                    harness.raw(&plain).await,
                    "{name}: identity composition differs"
                );
            }
        }
    });
}

#[test]
fn graded_frames_match_the_cpu_reference_on_bars_ramps_and_grid() {
    let Some(harness) =
        Harness::new("graded_frames_match_the_cpu_reference_on_bars_ramps_and_grid")
    else {
        return;
    };
    assert_eq!(harness.grade.lut_edge(), DEFAULT_NATIVE_GRADE_LUT_EDGE);
    block_on(async {
        let mut overall = ErrorStats::default();
        for (name, fixture) in fixtures() {
            let source = harness.import(&fixture).await;
            let pixels = rgb_pixels(&fixture);
            // Baseline: the ungraded import/output round trip.
            let mut round_trip = ErrorStats::default();
            for (actual, source) in harness.display(source.texture()).await.iter().zip(&pixels) {
                round_trip.add(*actual, *source, opaque(*source));
            }
            round_trip.assert_within_budget(&format!("{name} / ungraded round trip"));
            for (case, grade) in graded_cases() {
                let graded = harness
                    .grade
                    .grade_frame(&harness.context, &grade, source.texture())
                    .await
                    .unwrap()
                    .expect("non-identity grades render");
                assert_eq!(
                    (graded.width(), graded.height(), graded.format()),
                    (
                        fixture.width(),
                        fixture.height(),
                        TextureFormat::Rgba16Float
                    )
                );
                let actual = harness.display(&graded).await;
                let mut stats = ErrorStats::default();
                for (actual, source) in actual.iter().zip(&pixels) {
                    let expected = grade.apply_rgb8(*source);
                    stats.add(*actual, expected, opaque(*source));
                    overall.add(*actual, expected, opaque(*source));
                }
                stats.assert_within_budget(&format!("{name} / {case}"));
            }
        }
        overall.assert_within_budget("all fixtures and grades");
    });
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

fn half_pixels(bytes: &[u8]) -> Vec<[f32; 4]> {
    bytes
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

#[test]
fn translucent_layers_grade_straight_colour_and_keep_alpha() {
    let Some(harness) = Harness::new("translucent_layers_grade_straight_colour_and_keep_alpha")
    else {
        return;
    };
    block_on(async {
        const ALPHAS: [u8; 6] = [0, 32, 96, 160, 224, 255];
        let opaque = grid();
        let pixels = opaque
            .pixels()
            .as_chunks::<4>()
            .0
            .iter()
            .enumerate()
            .map(|(index, pixel)| [pixel[0], pixel[1], pixel[2], ALPHAS[index % ALPHAS.len()]])
            .collect::<Vec<_>>();
        let fixture = image(opaque.width(), opaque.height(), &pixels);
        let source = harness.import(&fixture).await;
        let renderer = NativeCompositionRenderer::new(&harness.context)
            .await
            .unwrap();
        for (case, grade) in graded_cases() {
            // Through the composition path: one full-canvas graded layer over a
            // transparent background.
            let mut scene =
                Scene::new(fixture.width(), fixture.height(), Rgba8::new(0, 0, 0, 0)).unwrap();
            scene.push_layer(
                SourceLayer::new(
                    SourceId::new(7),
                    0,
                    Transform::new(0, 0, fixture.width(), fixture.height(), Rotation::Deg0),
                )
                .with_grade(grade.clone()),
            );
            let plan = compile_scene(&scene, OutputTarget::Program).unwrap().0;
            let composed = renderer
                .render(
                    &harness.context,
                    &plan,
                    &[NativeSourceFrame::new(SourceId::new(7), source.texture())],
                )
                .await
                .unwrap();
            let mut stats = ErrorStats::default();
            for (actual, source) in half_pixels(&harness.raw(&composed).await)
                .into_iter()
                .zip(&pixels)
            {
                let (rgb, alpha) = straight_display(actual);
                assert_eq!(alpha, source[3], "{case}: alpha changed");
                if source[3] == 0 {
                    assert_eq!(rgb, [0; 3], "{case}: transparent pixel gained colour");
                    continue;
                }
                stats.add(
                    rgb,
                    grade.apply_rgb8([source[0], source[1], source[2]]),
                    *source,
                );
            }
            stats.assert_within_budget(&format!("translucent grid / {case}"));
        }
    });
}

#[test]
fn composed_graded_layers_match_cpu_composition() {
    let Some(harness) = Harness::new("composed_graded_layers_match_cpu_composition") else {
        return;
    };
    block_on(async {
        let renderer = NativeCompositionRenderer::new(&harness.context)
            .await
            .unwrap();
        let ramp = ramps();
        let bar = bars();
        let ramp_source = harness.import(&ramp).await;
        let bar_source = harness.import(&bar).await;
        let warm = compiled(&BUILTIN_PRESETS[1].grade, None);
        let full = primary_curves_lut_grade();
        let (width, height) = (ramp.width(), 12);
        let mut scene = Scene::new(width, height, Rgba8::new(0, 0, 0, 255)).unwrap();
        // A graded, scaled ramp; an ungraded bar strip; the same ramp graded
        // differently and rotated; then the bars graded again (cache reuse).
        scene.push_layer(
            SourceLayer::new(
                SourceId::new(1),
                0,
                Transform::new(0, 0, width, 5, Rotation::Deg0),
            )
            .with_grade(full.clone()),
        );
        scene.push_layer(SourceLayer::new(
            SourceId::new(2),
            1,
            Transform::new(0, 5, 128, 2, Rotation::Deg0),
        ));
        scene.push_layer(
            SourceLayer::new(
                SourceId::new(1),
                2,
                Transform::new(128, 5, 5, 256, Rotation::Deg270),
            )
            .with_grade(warm.clone()),
        );
        scene.push_layer(
            SourceLayer::new(
                SourceId::new(2),
                3,
                Transform::new(0, 7, 64, 5, Rotation::Deg0),
            )
            .with_grade(full.clone()),
        );
        let plan = compile_scene(&scene, OutputTarget::Program).unwrap().0;
        let expected = execute_cpu(
            &plan,
            &[
                CpuSourceFrame::new(SourceId::new(1), &ramp),
                CpuSourceFrame::new(SourceId::new(2), &bar),
            ],
        )
        .unwrap();
        let sources = [
            NativeSourceFrame::new(SourceId::new(1), ramp_source.texture()),
            NativeSourceFrame::new(SourceId::new(2), bar_source.texture()),
        ];
        // Twice, so the second frame uses cached LUTs.
        for frame in 0..2 {
            let composed = renderer
                .render(&harness.context, &plan, &sources)
                .await
                .unwrap();
            let actual = harness.display(&composed).await;
            let mut stats = ErrorStats::default();
            for (actual, expected) in actual.iter().zip(rgb_pixels(&expected)) {
                stats.add(*actual, expected, opaque(expected));
            }
            stats.assert_within_budget(&format!("composition frame {frame}"));
        }
    });
}

/// CPU emulation of the shader's manual trilinear lookup into `bake(edge)`.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
fn emulate_trilinear(grid: &[[f32; 3]], edge: usize, rgb: [u8; 3]) -> [u8; 3] {
    let last = (edge - 1) as f32;
    let position = rgb.map(|code| f32::from(code) / 255.0 * last);
    let base = position.map(|value| (value.floor() as usize).min(edge - 2));
    let t: [f32; 3] = core::array::from_fn(|axis| position[axis] - base[axis] as f32);
    let texel = |dr: usize, dg: usize, db: usize, channel: usize| {
        grid[((base[2] + db) * edge + base[1] + dg) * edge + base[0] + dr][channel]
    };
    core::array::from_fn(|channel| {
        let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
        let c00 = lerp(texel(0, 0, 0, channel), texel(1, 0, 0, channel), t[0]);
        let c10 = lerp(texel(0, 1, 0, channel), texel(1, 1, 0, channel), t[0]);
        let c01 = lerp(texel(0, 0, 1, channel), texel(1, 0, 1, channel), t[0]);
        let c11 = lerp(texel(0, 1, 1, channel), texel(1, 1, 1, channel), t[0]);
        let value = lerp(lerp(c00, c10, t[1]), lerp(c01, c11, t[1]), t[2]);
        (value.clamp(0.0, 1.0) * 255.0).round() as u8
    })
}

#[test]
fn full_stack_grades_stay_within_the_lattice_interpolation_bound() {
    let Some(harness) =
        Harness::new("full_stack_grades_stay_within_the_lattice_interpolation_bound")
    else {
        return;
    };
    block_on(async {
        for (name, fixture) in fixtures() {
            let source = harness.import(&fixture).await;
            let pixels = rgb_pixels(&fixture);
            for (case, grade) in stress_cases() {
                let grid = grade.bake(harness.grade.lut_edge());
                let graded = harness
                    .grade
                    .grade_frame(&harness.context, &grade, source.texture())
                    .await
                    .unwrap()
                    .expect("non-identity grades render");
                let actual = harness.display(&graded).await;
                let mut reference = ErrorStats::default();
                let mut emulation = ErrorStats::default();
                for (actual, source) in actual.iter().zip(&pixels) {
                    reference.add(*actual, grade.apply_rgb8(*source), opaque(*source));
                    emulation.add(
                        *actual,
                        emulate_trilinear(&grid, harness.grade.lut_edge(), *source),
                        opaque(*source),
                    );
                }
                reference.assert_within(
                    &format!("{name} / {case} vs reference"),
                    STRESS_MAX_ERROR,
                    MEAN_ERROR,
                );
                emulation.assert_within(
                    &format!("{name} / {case} vs CPU trilinear"),
                    EMULATION_MAX_ERROR,
                    MEAN_ERROR,
                );
            }
        }
    });
}

/// A scene layer grade (SW-019) runs as a second LUT pass after the
/// source's grade, matching the CPU chain (each grade quantized to 8-bit
/// there) within twice the single-grade budget.
#[test]
fn chained_source_and_layer_grades_match_cpu_composition() {
    let Some(harness) = Harness::new("chained_source_and_layer_grades_match_cpu_composition")
    else {
        return;
    };
    block_on(async {
        let renderer = NativeCompositionRenderer::new(&harness.context)
            .await
            .unwrap();
        let ramp = ramps();
        let source = harness.import(&ramp).await;
        let warm = compiled(&BUILTIN_PRESETS[1].grade, None);
        let full = primary_curves_lut_grade();
        let (width, height) = (ramp.width(), ramp.height());
        let mut scene = Scene::new(width, height, Rgba8::new(0, 0, 0, 255)).unwrap();
        scene.push_layer(
            SourceLayer::new(
                SourceId::new(1),
                0,
                Transform::new(0, 0, width, height, Rotation::Deg0),
            )
            .with_grade(warm.clone())
            .with_layer_grade(full.clone()),
        );
        let plan = compile_scene(&scene, OutputTarget::Program).unwrap().0;
        let expected = execute_cpu(&plan, &[CpuSourceFrame::new(SourceId::new(1), &ramp)]).unwrap();
        let composed = renderer
            .render(
                &harness.context,
                &plan,
                &[NativeSourceFrame::new(SourceId::new(1), source.texture())],
            )
            .await
            .unwrap();
        let actual = harness.display(&composed).await;
        let mut stats = ErrorStats::default();
        for (actual, expected) in actual.iter().zip(rgb_pixels(&expected)) {
            stats.add(*actual, expected, opaque(expected));
        }
        stats.assert_within("chained grades", MAX_ERROR * 2, MEAN_ERROR * 2.0);
    });
}

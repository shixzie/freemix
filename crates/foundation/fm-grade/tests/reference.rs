// The assertions compare exact reference values.
#![allow(clippy::float_cmp)]

use std::sync::Arc;

use fm_grade::reference::{self, evaluate_pixel, normalize};
use fm_grade::{
    BUILTIN_PRESETS, CompiledGrade, Contrast, CubeLut, Curve, CurveChannel, CurvePoint, Grade,
    GradeError, GradeLut, HueCurve, HueCurveKind, HueCurves, HuePoint, RgbCurves, Wheel, WheelKind,
    Wheels, WhiteBalance, builtin_preset,
};

/// Pixels exercised by the per-stage goldens.
const PIXELS: [[u8; 3]; 6] = [
    [0, 0, 0],
    [32, 64, 96],
    [128, 128, 128],
    [200, 100, 50],
    [255, 255, 255],
    [10, 240, 130],
];

fn grade(mutate: impl FnOnce(&mut Grade)) -> Grade {
    let mut grade = Grade::IDENTITY;
    mutate(&mut grade);
    grade
}

fn compile(grade: &Grade) -> CompiledGrade {
    CompiledGrade::new(grade, None).unwrap()
}

/// Asserts both the direct and compiled paths match independently computed
/// (f64) goldens within one LSB.
fn assert_golden(name: &str, grade: &Grade, expected: [[u8; 3]; 6]) {
    let compiled = compile(grade);
    for (pixel, expected) in PIXELS.iter().zip(expected) {
        let direct = evaluate_pixel(grade, None, *pixel);
        assert_eq!(direct, compiled.apply_rgb8(*pixel), "{name} {pixel:?}");
        for channel in 0..3 {
            assert!(
                direct[channel].abs_diff(expected[channel]) <= 1,
                "{name} {pixel:?}: got {direct:?}, expected {expected:?}"
            );
        }
    }
}

fn wheels(mutate: impl FnOnce(&mut Wheels)) -> Grade {
    grade(|grade| mutate(&mut grade.wheels))
}

fn every_code_triplet(step: usize) -> impl Iterator<Item = [u8; 3]> {
    let codes = move || (0..=255_u8).step_by(step).chain([255]);
    codes().flat_map(move |r| codes().flat_map(move |g| codes().map(move |b| [r, g, b])))
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

fn warming_lut() -> CubeLut {
    let mut entries = Vec::new();
    for b in 0..3_u8 {
        for g in 0..3_u8 {
            for r in 0..3_u8 {
                let value = |code: u8| f32::from(code) / 2.0;
                entries.push([(value(r) * 1.1).min(1.0), value(g), value(b) * 0.8]);
            }
        }
    }
    CubeLut::new_3d(3, entries, [0.0; 3], [1.0; 3]).unwrap()
}

#[test]
fn identity_and_disabled_grades_return_source_bytes_exactly() {
    let identity = compile(&Grade::IDENTITY);
    assert!(identity.is_identity());
    let disabled_grade = Grade {
        enabled: false,
        ..full_grade()
    };
    let disabled = CompiledGrade::new(&disabled_grade, Some(Arc::new(warming_lut()))).unwrap();
    assert!(disabled.is_identity());
    assert!(!disabled.lut_missing());
    for pixel in every_code_triplet(7) {
        assert_eq!(identity.apply_rgb8(pixel), pixel);
        assert_eq!(disabled.apply_rgb8(pixel), pixel);
        assert_eq!(evaluate_pixel(&Grade::IDENTITY, None, pixel), pixel);
        assert_eq!(evaluate_pixel(&disabled_grade, None, pixel), pixel);
    }
    let mut row = vec![1, 2, 3, 4, 250, 128, 7, 0, 9];
    identity.apply_rgba8_in_place(&mut row);
    assert_eq!(row, vec![1, 2, 3, 4, 250, 128, 7, 0, 9]);
}

#[test]
fn neutral_values_with_non_canonical_shape_are_identity() {
    // A contrast pivot alone, zero-valued hue curves, and a zero-strength
    // LUT are all exact identities.
    let neutral = grade(|grade| {
        grade.contrast = Contrast::new(0, 1234);
        grade.hue_curves.hue_vs_hue = HueCurve::new(vec![HuePoint::new(100, 0)]);
        grade.lut = Some(GradeLut::new("asset://luts/a.cube", 0));
    });
    assert!(neutral.is_identity());
    assert!(compile(&neutral).is_identity());
}

#[test]
fn a_missing_lut_is_bypassed_and_reported() {
    let only_lut = grade(|grade| grade.lut = Some(GradeLut::new("asset://luts/a.cube", 500)));
    assert!(!only_lut.is_identity());
    let compiled = compile(&only_lut);
    assert!(compiled.lut_missing());
    assert!(compiled.is_identity());
    for pixel in every_code_triplet(17) {
        assert_eq!(compiled.apply_rgb8(pixel), pixel);
        assert_eq!(evaluate_pixel(&only_lut, None, pixel), pixel);
    }
}

#[test]
fn white_balance_and_exposure_match_goldens() {
    assert_golden(
        "temperature +500",
        &grade(|grade| grade.white_balance = WhiteBalance::new(500, 0)),
        [
            [0, 0, 0],
            [35, 63, 87],
            [137, 126, 117],
            [213, 99, 45],
            [255, 252, 233],
            [11, 237, 118],
        ],
    );
    assert_golden(
        "tint -400",
        &grade(|grade| grade.white_balance = WhiteBalance::new(0, -400)),
        [
            [0, 0, 0],
            [30, 65, 91],
            [122, 130, 122],
            [191, 102, 47],
            [244, 255, 244],
            [9, 244, 124],
        ],
    );
    assert_golden(
        "exposure +1 stop",
        &grade(|grade| grade.exposure_millistops = 1000),
        [
            [0, 0, 0],
            [47, 90, 133],
            [176, 176, 176],
            [255, 138, 71],
            [255, 255, 255],
            [18, 255, 178],
        ],
    );
    assert_golden(
        "exposure -2.5 stops",
        &grade(|grade| grade.exposure_millistops = -2500),
        [
            [0, 0, 0],
            [8, 24, 39],
            [55, 55, 55],
            [90, 41, 17],
            [117, 117, 117],
            [2, 109, 56],
        ],
    );
}

#[test]
fn white_balance_preserves_neutral_luminance() {
    // Gains are normalized by their BT.709 luma, so linear-light grey keeps
    // its luminance under any temperature/tint.
    for (temperature, tint) in [(1000, 0), (-1000, 0), (0, 1000), (600, -700)] {
        let graded =
            reference::white_balance_exposure([0.5; 3], WhiteBalance::new(temperature, tint), 0);
        let linear = graded.map(reference::srgb_eotf);
        let luminance = reference::luma(linear);
        assert!(
            (luminance - reference::srgb_eotf(0.5)).abs() < 1e-5,
            "{temperature}/{tint}: {luminance}"
        );
    }
}

#[test]
fn wheels_match_goldens() {
    assert_golden(
        "lift master",
        &wheels(|wheels| wheels.lift = Wheel::new(0, 0, 200)),
        [
            [13, 13, 13],
            [43, 74, 104],
            [134, 134, 134],
            [203, 108, 60],
            [255, 255, 255],
            [22, 241, 136],
        ],
    );
    assert_golden(
        "gamma master",
        &wheels(|wheels| wheels.gamma = Wheel::new(0, 0, -300)),
        [
            [0, 0, 0],
            [20, 46, 77],
            [109, 109, 109],
            [189, 81, 34],
            [255, 255, 255],
            [5, 237, 111],
        ],
    );
    assert_golden(
        "gain colour",
        &wheels(|wheels| wheels.gain = Wheel::new(300, -200, 100)),
        [
            [0, 0, 0],
            [32, 69, 117],
            [126, 138, 156],
            [197, 108, 61],
            [252, 255, 255],
            [10, 255, 159],
        ],
    );
    assert_golden(
        "offset colour",
        &wheels(|wheels| wheels.offset = Wheel::new(0, 500, 0)),
        [
            [20, 0, 0],
            [52, 58, 96],
            [148, 122, 128],
            [220, 94, 50],
            [255, 249, 255],
            [30, 234, 130],
        ],
    );
    assert_golden(
        "lift colour",
        &wheels(|wheels| wheels.lift = Wheel::new(-400, 600, -100)),
        [
            [18, 0, 0],
            [47, 55, 80],
            [137, 122, 115],
            [204, 93, 30],
            [255, 255, 255],
            [27, 239, 118],
        ],
    );
}

#[test]
fn contrast_and_saturation_match_goldens() {
    assert_golden(
        "contrast +500 pivot 0.4",
        &grade(|grade| grade.contrast = Contrast::new(500, 4000)),
        [
            [0, 0, 0],
            [0, 45, 93],
            [141, 141, 141],
            [249, 99, 24],
            [255, 255, 255],
            [0, 255, 144],
        ],
    );
    assert_golden(
        "saturation 0",
        &grade(|grade| grade.saturation_permille = 0),
        [
            [0, 0, 0],
            [60, 60, 60],
            [128, 128, 128],
            [118, 118, 118],
            [255, 255, 255],
            [183, 183, 183],
        ],
    );
    assert_golden(
        "saturation 2000",
        &grade(|grade| grade.saturation_permille = 2000),
        [
            [0, 0, 0],
            [4, 68, 132],
            [128, 128, 128],
            [255, 82, 0],
            [255, 255, 255],
            [0, 255, 77],
        ],
    );
}

#[test]
fn rgb_curves_match_goldens() {
    let curve = Curve::new(vec![
        CurvePoint::new(0, 0),
        CurvePoint::new(5000, 7000),
        CurvePoint::new(10_000, 10_000),
    ]);
    assert_golden(
        "master curve",
        &grade(|grade| grade.curves.master = curve.clone()),
        [
            [0, 0, 0],
            [47, 96, 142],
            [179, 179, 179],
            [227, 147, 75],
            [255, 255, 255],
            [14, 247, 181],
        ],
    );
    // The same curve on one channel leaves the others untouched.
    let red_only = grade(|grade| grade.curves.red = curve);
    let pixel = evaluate_pixel(&red_only, None, [200, 100, 50]);
    assert_eq!(pixel, [227, 100, 50]);
}

#[test]
fn hue_curves_rotate_hue_and_scale_saturation() {
    let rotate = grade(|grade| {
        grade.hue_curves.hue_vs_hue = HueCurve::new(vec![HuePoint::new(1200, 500)]);
    });
    // +90°: pure red (h = 0) becomes h = 0.25, i.e. (0.5, 1, 0).
    assert_eq!(evaluate_pixel(&rotate, None, [255, 0, 0]), [128, 255, 0]);
    // Greys have S = 0 and are untouched.
    assert_eq!(evaluate_pixel(&rotate, None, [90, 90, 90]), [90, 90, 90]);

    let desaturate = grade(|grade| {
        grade.hue_curves.hue_vs_saturation = HueCurve::new(vec![HuePoint::new(0, -1000)]);
    });
    assert_eq!(
        evaluate_pixel(&desaturate, None, [255, 0, 0]),
        [255, 255, 255]
    );
    assert_eq!(
        evaluate_pixel(&desaturate, None, [10, 200, 40]),
        [200, 200, 200]
    );

    // Luma-vs-saturation: dark pixels lose all saturation, bright ones keep it.
    let dark_only = grade(|grade| {
        grade.hue_curves.luma_vs_saturation =
            HueCurve::new(vec![HuePoint::new(2000, -1000), HuePoint::new(3000, 0)]);
    });
    assert_eq!(evaluate_pixel(&dark_only, None, [40, 0, 0]), [40, 40, 40]);
    assert_eq!(
        evaluate_pixel(&dark_only, None, [255, 255, 0]),
        [255, 255, 0]
    );

    // Doubling saturation is clamped to S = 1.
    let boost = grade(|grade| {
        grade.hue_curves.hue_vs_saturation = HueCurve::new(vec![HuePoint::new(0, 1000)]);
    });
    assert_eq!(evaluate_pixel(&boost, None, [200, 100, 100]), [200, 0, 0]);
}

#[test]
fn lut_stage_mixes_by_strength() {
    let lut = Arc::new(warming_lut());
    let full = grade(|grade| grade.lut = Some(GradeLut::new("asset://luts/warm.cube", 1000)));
    let half = grade(|grade| grade.lut = Some(GradeLut::new("asset://luts/warm.cube", 500)));
    let compiled_full = CompiledGrade::new(&full, Some(lut.clone())).unwrap();
    let compiled_half = CompiledGrade::new(&half, Some(lut.clone())).unwrap();
    assert!(!compiled_full.lut_missing());
    // The cube is linear per axis, so trilinear sampling is exact:
    // r * 1.1 (clamped), g, b * 0.8.
    assert_eq!(compiled_full.apply_rgb8([100, 100, 100]), [110, 100, 80]);
    assert_eq!(compiled_half.apply_rgb8([100, 100, 100]), [105, 100, 90]);
    for pixel in every_code_triplet(13) {
        assert_eq!(
            compiled_half.apply_rgb8(pixel),
            evaluate_pixel(&half, Some(&lut), pixel)
        );
    }
}

#[test]
fn compiled_primary_tables_equal_direct_stage_evaluation() {
    let grade = full_grade();
    let compiled = CompiledGrade::new(&grade, Some(Arc::new(warming_lut()))).unwrap();
    for code in 0..=255_u8 {
        let mut direct = [normalize(code); 3];
        direct = reference::white_balance_exposure(
            direct,
            grade.white_balance,
            grade.exposure_millistops,
        );
        direct = reference::wheels(direct, &grade.wheels);
        direct = reference::contrast(direct, grade.contrast);
        for (channel, (table, direct)) in compiled.primary_tables().iter().zip(direct).enumerate() {
            assert_eq!(
                table[usize::from(code)].to_bits(),
                direct.to_bits(),
                "channel {channel} code {code}"
            );
        }
    }
}

#[test]
fn compiled_evaluation_equals_direct_evaluation_bit_exactly() {
    let lut = Arc::new(warming_lut());
    let primary_only = Grade {
        saturation_permille: 1000,
        curves: RgbCurves::IDENTITY,
        hue_curves: HueCurves::IDENTITY,
        lut: None,
        ..full_grade()
    };
    for grade in [full_grade(), primary_only] {
        let compiled = CompiledGrade::new(&grade, Some(lut.clone())).unwrap();
        for pixel in every_code_triplet(15) {
            assert_eq!(
                compiled.apply_rgb8(pixel),
                evaluate_pixel(&grade, Some(&lut), pixel),
                "{pixel:?}"
            );
        }
    }
}

#[test]
fn rgba_application_leaves_alpha_untouched() {
    let compiled = compile(&grade(|grade| grade.saturation_permille = 0));
    let mut pixels = vec![255, 0, 0, 17, 0, 255, 0, 0, 0, 0, 255, 255, 9];
    compiled.apply_rgba8_in_place(&mut pixels);
    assert_eq!(&pixels[3..4], &[17]);
    assert_eq!(&pixels[7..8], &[0]);
    assert_eq!(&pixels[11..], &[255, 9]);
    assert_eq!(
        &pixels[..3],
        &evaluate_pixel(compiled.grade(), None, [255, 0, 0])
    );
}

#[test]
fn bake_samples_the_continuous_function_red_fastest() {
    let identity = compile(&Grade::IDENTITY).bake(3);
    assert_eq!(identity.len(), 27);
    assert_eq!(identity[1], [0.5, 0.0, 0.0]);
    assert_eq!(identity[3], [0.0, 0.5, 0.0]);
    assert_eq!(identity[9], [0.0, 0.0, 0.5]);
    assert_eq!(identity[26], [1.0, 1.0, 1.0]);

    let compiled = CompiledGrade::new(&full_grade(), Some(Arc::new(warming_lut()))).unwrap();
    let baked = compiled.bake(5);
    assert_eq!(baked.len(), 125);
    assert_eq!(
        baked[(3 * 5 + 2) * 5 + 1],
        compiled.evaluate([0.25, 0.5, 0.75])
    );
    assert_eq!(
        compiled.evaluate([0.25, 0.5, 0.75]),
        reference::evaluate(
            compiled.grade(),
            compiled.lut().map(|lut| &**lut),
            [0.25, 0.5, 0.75]
        )
    );
    assert!(
        baked
            .iter()
            .flatten()
            .all(|value| (0.0..=1.0).contains(value))
    );
    // Edges are clamped to 2..=65.
    assert_eq!(compiled.bake(0).len(), 8);
    assert_eq!(compiled.bake(1000).len(), 65 * 65 * 65);
}

#[test]
#[allow(clippy::too_many_lines)]
fn every_grade_bound_is_validated_with_a_typed_error() {
    let cases: Vec<(Grade, GradeError)> = vec![
        (
            grade(|g| g.white_balance.temperature = 1001),
            GradeError::Temperature(1001),
        ),
        (
            grade(|g| g.white_balance.tint = -1001),
            GradeError::Tint(-1001),
        ),
        (
            grade(|g| g.exposure_millistops = 4001),
            GradeError::Exposure(4001),
        ),
        (
            grade(|g| g.contrast.amount = -1001),
            GradeError::ContrastAmount(-1001),
        ),
        (
            grade(|g| g.contrast.pivot_bp = 10_001),
            GradeError::ContrastPivot(10_001),
        ),
        (
            grade(|g| g.saturation_permille = 3001),
            GradeError::Saturation(3001),
        ),
        (
            grade(|g| g.wheels.gamma = Wheel::new(800, 601, 0)),
            GradeError::WheelRadius {
                wheel: WheelKind::Gamma,
                x: 800,
                y: 601,
            },
        ),
        (
            grade(|g| g.wheels.offset.master = 1001),
            GradeError::WheelMaster {
                wheel: WheelKind::Offset,
                master: 1001,
            },
        ),
        (
            grade(|g| g.curves.red = Curve::new(vec![CurvePoint::new(0, 0)])),
            GradeError::CurvePointCount {
                channel: CurveChannel::Red,
                count: 1,
            },
        ),
        (
            grade(|g| {
                g.curves.green = Curve::new(
                    (0..17_u16)
                        .map(|index| CurvePoint::new(index * 625, 0))
                        .collect(),
                );
            }),
            GradeError::CurvePointCount {
                channel: CurveChannel::Green,
                count: 17,
            },
        ),
        (
            grade(|g| {
                g.curves.blue =
                    Curve::new(vec![CurvePoint::new(0, 0), CurvePoint::new(10_000, 10_001)]);
            }),
            GradeError::CurveCoordinate {
                channel: CurveChannel::Blue,
                index: 1,
            },
        ),
        (
            grade(|g| {
                g.curves.master = Curve::new(vec![
                    CurvePoint::new(0, 0),
                    CurvePoint::new(5000, 1),
                    CurvePoint::new(5000, 2),
                    CurvePoint::new(10_000, 10_000),
                ]);
            }),
            GradeError::CurveNotIncreasing {
                channel: CurveChannel::Master,
                index: 2,
            },
        ),
        (
            grade(|g| {
                g.curves.master =
                    Curve::new(vec![CurvePoint::new(1, 0), CurvePoint::new(10_000, 1)]);
            }),
            GradeError::CurveStart {
                channel: CurveChannel::Master,
            },
        ),
        (
            grade(|g| {
                g.curves.master = Curve::new(vec![CurvePoint::new(0, 0), CurvePoint::new(9999, 1)]);
            }),
            GradeError::CurveEnd {
                channel: CurveChannel::Master,
            },
        ),
        (
            grade(|g| {
                g.hue_curves.hue_vs_hue =
                    HueCurve::new((0..17_u16).map(|index| HuePoint::new(index, 1)).collect());
            }),
            GradeError::HueCurvePointCount {
                curve: HueCurveKind::HueVsHue,
                count: 17,
            },
        ),
        (
            grade(|g| g.hue_curves.hue_vs_saturation = HueCurve::new(vec![HuePoint::new(3600, 1)])),
            GradeError::HueCurvePosition {
                curve: HueCurveKind::HueVsSaturation,
                index: 0,
                position: 3600,
            },
        ),
        (
            grade(|g| {
                g.hue_curves.luma_vs_saturation = HueCurve::new(vec![HuePoint::new(10_001, 1)]);
            }),
            GradeError::HueCurvePosition {
                curve: HueCurveKind::LumaVsSaturation,
                index: 0,
                position: 10_001,
            },
        ),
        (
            grade(|g| {
                g.hue_curves.hue_vs_hue =
                    HueCurve::new(vec![HuePoint::new(0, 0), HuePoint::new(5, 1001)]);
            }),
            GradeError::HueCurveValue {
                curve: HueCurveKind::HueVsHue,
                index: 1,
                value: 1001,
            },
        ),
        (
            grade(|g| {
                g.hue_curves.hue_vs_hue =
                    HueCurve::new(vec![HuePoint::new(5, 0), HuePoint::new(5, 1)]);
            }),
            GradeError::HueCurveNotIncreasing {
                curve: HueCurveKind::HueVsHue,
                index: 1,
            },
        ),
        (
            grade(|g| g.lut = Some(GradeLut::new("file:///luts/a.cube", 1))),
            GradeError::LutUriScheme,
        ),
        (
            grade(|g| g.lut = Some(GradeLut::new("asset://luts/", 1))),
            GradeError::LutUriPath,
        ),
        (
            grade(|g| g.lut = Some(GradeLut::new("asset://luts/../secret.cube", 1))),
            GradeError::LutUriPath,
        ),
        (
            grade(|g| {
                g.lut = Some(GradeLut::new(
                    format!("asset://luts/{}", "a".repeat(1012)),
                    1,
                ));
            }),
            GradeError::LutUriTooLong { bytes: 1025 },
        ),
        (
            grade(|g| g.lut = Some(GradeLut::new("asset://luts/a.cube", 1001))),
            GradeError::LutStrength(1001),
        ),
    ];
    for (grade, expected) in cases {
        assert_eq!(grade.validate(), Err(expected));
        assert_eq!(CompiledGrade::new(&grade, None).unwrap_err(), expected);
        assert_ne!(expected.to_string(), "");
    }

    // Inclusive bounds are accepted.
    let edges = Grade {
        enabled: true,
        white_balance: WhiteBalance::new(-1000, 1000),
        exposure_millistops: -4000,
        contrast: Contrast::new(1000, 10_000),
        saturation_permille: 3000,
        wheels: Wheels {
            lift: Wheel::new(600, 800, -1000),
            gamma: Wheel::new(-1000, 0, 1000),
            gain: Wheel::new(0, 1000, 1000),
            offset: Wheel::new(0, -1000, -1000),
        },
        curves: RgbCurves::IDENTITY,
        hue_curves: HueCurves {
            hue_vs_hue: HueCurve::new(vec![HuePoint::new(0, -1000), HuePoint::new(3599, 1000)]),
            hue_vs_saturation: HueCurve::EMPTY,
            luma_vs_saturation: HueCurve::new(vec![HuePoint::new(10_000, 1000)]),
        },
        lut: Some(GradeLut::new(
            format!("asset://luts/{}", "a".repeat(1011)),
            1000,
        )),
    };
    assert_eq!(edges.validate(), Ok(()));
    let compiled = compile(&edges);
    for pixel in every_code_triplet(51) {
        let _ = compiled.apply_rgb8(pixel);
    }
}

#[test]
fn extreme_wheel_directions_are_rejected_without_overflow() {
    // `x² + y²` for two `i16::MIN` components exceeds `i32::MAX`.
    for (x, y) in [
        (i16::MIN, i16::MIN),
        (i16::MAX, i16::MAX),
        (i16::MIN, 0),
        (0, i16::MAX),
    ] {
        assert_eq!(
            grade(|g| g.wheels.lift = Wheel::new(x, y, 0)).validate(),
            Err(GradeError::WheelRadius {
                wheel: WheelKind::Lift,
                x,
                y,
            })
        );
    }
}

#[test]
fn builtin_presets_are_valid_stable_and_distinct() {
    let ids: Vec<&str> = BUILTIN_PRESETS.iter().map(|preset| preset.id).collect();
    assert_eq!(ids, ["neutral", "warm", "cool", "punchy", "faded", "bw"]);
    for preset in &BUILTIN_PRESETS {
        assert_eq!(preset.grade.validate(), Ok(()), "{}", preset.id);
        assert_eq!(builtin_preset(preset.id), Some(preset));
        assert_eq!(preset.grade.is_identity(), preset.id == "neutral");
    }
    assert!(builtin_preset("missing").is_none());

    let bw = compile(&builtin_preset("bw").unwrap().grade);
    let [r, g, b] = bw.apply_rgb8([200, 40, 90]);
    assert!(r == g && g == b);
    let warm = compile(&builtin_preset("warm").unwrap().grade).apply_rgb8([128, 128, 128]);
    assert!(warm[0] > warm[2]);
    let cool = compile(&builtin_preset("cool").unwrap().grade).apply_rgb8([128, 128, 128]);
    assert!(cool[2] > cool[0]);
    let faded = compile(&builtin_preset("faded").unwrap().grade);
    assert!(faded.apply_rgb8([0, 0, 0])[0] > 0);
    assert!(faded.apply_rgb8([255, 255, 255])[0] < 255);
}

//! SW-002 transition family: CPU reference goldens for the vertical wipe and
//! slide, Fly, `FlyRotate`, Cube, `CubeZoom`, `CrossZoom`, and Merge.
//!
//! Each kind has byte-exact endpoints, exact pixels at representative
//! progress values on small odd-sized fixtures, and FNV-1a checksums of whole
//! frames so any change to the reference arithmetic is caught.

use fm_compositor::{Rgba8, TransitionError, TransitionKind, TransitionPlan, execute_transition};
use fm_video::ImageFrame;

const NEW_KINDS: [TransitionKind; 8] = [
    TransitionKind::VerticalWipe,
    TransitionKind::VerticalSlide,
    TransitionKind::Fly,
    TransitionKind::FlyRotate,
    TransitionKind::Cube,
    TransitionKind::CubeZoom,
    TransitionKind::CrossZoom,
    TransitionKind::Merge,
];

/// A premultiplied frame with distinct texels, some partly transparent, and
/// `padding` spare bytes per row.
fn pattern(width: u32, height: u32, seed: u32, padding: usize) -> ImageFrame {
    let stride = width as usize * 4 + padding;
    let mut pixels = vec![0xA5; stride * height as usize];
    for y in 0..height {
        for x in 0..width {
            let alpha = match (x + 2 * y + seed) % 4 {
                0 => 128,
                1 => 64,
                _ => 255,
            };
            let channel = |value: u32| u8::try_from((value % 256) * alpha / 255).unwrap();
            let offset = y as usize * stride + x as usize * 4;
            pixels[offset..offset + 4].copy_from_slice(&[
                channel(x * 23 + seed * 61),
                channel(y * 37 + seed * 17),
                channel((x + y) * 13 + seed * 91),
                u8::try_from(alpha).unwrap(),
            ]);
        }
    }
    ImageFrame::new(width, height, stride, pixels).unwrap()
}

fn solid(width: u32, height: u32, color: Rgba8) -> ImageFrame {
    ImageFrame::new(
        width,
        height,
        width as usize * 4,
        (0..width * height).flat_map(|_| color.to_bytes()).collect(),
    )
    .unwrap()
}

/// Opaque columns: `left` for `x < split`, `right` from `split` on.
fn columns(width: u32, height: u32, split: u32, left: Rgba8, right: Rgba8) -> ImageFrame {
    let mut pixels = Vec::new();
    for _ in 0..height {
        for x in 0..width {
            pixels.extend_from_slice(&if x < split { left } else { right }.to_bytes());
        }
    }
    ImageFrame::new(width, height, width as usize * 4, pixels).unwrap()
}

fn run(kind: TransitionKind, from: &ImageFrame, to: &ImageFrame, n: u32, d: u32) -> ImageFrame {
    execute_transition(TransitionPlan::compile(kind, n, d).unwrap(), from, to).unwrap()
}

fn pixel(frame: &ImageFrame, x: u32, y: u32) -> Rgba8 {
    frame.pixel(x, y).unwrap()
}

fn row(frame: &ImageFrame, y: u32) -> Vec<Rgba8> {
    (0..frame.width()).map(|x| pixel(frame, x, y)).collect()
}

fn fnv1a(frame: &ImageFrame) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for y in 0..frame.height() {
        for x in 0..frame.width() {
            for byte in pixel(frame, x, y).to_bytes() {
                hash ^= u64::from(byte);
                hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
    }
    hash
}

/// Premultiplied source-over with the reference's rounding.
fn over(source: Rgba8, destination: Rgba8) -> Rgba8 {
    let inverse = u16::from(255 - source.a);
    let channel = |source: u8, destination: u8| {
        let scaled = (u16::from(destination) * inverse + 127) / 255;
        source.saturating_add(u8::try_from(scaled).unwrap())
    };
    Rgba8::new(
        channel(source.r, destination.r),
        channel(source.g, destination.g),
        channel(source.b, destination.b),
        channel(source.a, destination.a),
    )
}

fn blend(left: u8, right: u8, n: u32, d: u32) -> u8 {
    u8::try_from((u32::from(left) * (d - n) + u32::from(right) * n + d / 2) / d).unwrap()
}

#[test]
fn every_new_kind_compiles_and_stinger_still_does_not() {
    for kind in NEW_KINDS {
        let plan = TransitionPlan::compile(kind, 2, 5).unwrap();
        assert_eq!(plan.kind(), kind);
        assert!(kind.shape().is_some());
        assert_eq!(
            TransitionPlan::compile(kind, 6, 5),
            Err(TransitionError::ProgressOutOfRange {
                numerator: 6,
                denominator: 5
            })
        );
    }
    assert_eq!(
        TransitionPlan::compile(TransitionKind::Stinger, 1, 2),
        Err(TransitionError::UnsupportedKind(TransitionKind::Stinger))
    );
}

#[test]
fn endpoints_are_byte_exact_clones_including_row_padding() {
    for (width, height, padding) in [(7, 5, 0), (13, 9, 12), (1, 1, 4), (2, 3, 0)] {
        let from = pattern(width, height, 0, padding);
        let to = pattern(width, height, 1, padding);
        for kind in NEW_KINDS {
            for denominator in [1, 2, 7] {
                assert_eq!(run(kind, &from, &to, 0, denominator), from, "{kind:?}");
                assert_eq!(
                    run(kind, &from, &to, denominator, denominator),
                    to,
                    "{kind:?}"
                );
            }
        }
    }
}

#[test]
fn mismatched_layouts_are_rejected() {
    let from = pattern(7, 5, 0, 0);
    for kind in NEW_KINDS {
        let plan = TransitionPlan::compile(kind, 1, 2).unwrap();
        assert!(matches!(
            execute_transition(plan, &from, &pattern(7, 6, 1, 0)),
            Err(TransitionError::Blend(
                fm_video::BlendError::HeightMismatch { left: 5, right: 6 }
            ))
        ));
        assert!(matches!(
            execute_transition(plan, &from, &pattern(7, 5, 1, 4)),
            Err(TransitionError::Blend(
                fm_video::BlendError::StrideMismatch { .. }
            ))
        ));
    }
}

#[test]
fn vertical_wipe_replaces_rows_from_the_top() {
    let (from, to) = (pattern(7, 5, 0, 0), pattern(7, 5, 1, 0));
    // floor(5 * 1/2) = 2 rows.
    let output = run(TransitionKind::VerticalWipe, &from, &to, 1, 2);
    for y in 0..5 {
        assert_eq!(row(&output, y), row(if y < 2 { &to } else { &from }, y));
    }
    // floor(5 * 1/6) = 0 rows: still Program.
    assert_eq!(run(TransitionKind::VerticalWipe, &from, &to, 1, 6), from);
}

#[test]
fn vertical_slide_pushes_program_up() {
    let (from, to) = (pattern(7, 5, 0, 0), pattern(7, 5, 1, 0));
    // Offset floor(5 * 2/5) = 2.
    let output = run(TransitionKind::VerticalSlide, &from, &to, 2, 5);
    assert_eq!(row(&output, 0), row(&from, 2));
    assert_eq!(row(&output, 2), row(&from, 4));
    assert_eq!(row(&output, 3), row(&to, 0));
    assert_eq!(row(&output, 4), row(&to, 1));
}

#[test]
fn merge_halves_meet_in_the_centre() {
    let (from, to) = (pattern(7, 5, 0, 0), pattern(7, 5, 1, 0));
    // L = 3; at 1/2 the left extent is 1 and the right extent floor(4/2) = 2.
    let output = run(TransitionKind::Merge, &from, &to, 1, 2);
    for y in 0..5 {
        assert_eq!(pixel(&output, 0, y), pixel(&to, 2, y));
        for x in 1..5 {
            assert_eq!(pixel(&output, x, y), pixel(&from, x, y));
        }
        assert_eq!(pixel(&output, 5, y), pixel(&to, 3, y));
        assert_eq!(pixel(&output, 6, y), pixel(&to, 4, y));
    }
    // At 4/5: left extent floor(12/5) = 2, right extent floor(16/5) = 3.
    let output = run(TransitionKind::Merge, &from, &to, 4, 5);
    for y in 0..5 {
        assert_eq!(pixel(&output, 0, y), pixel(&to, 1, y));
        assert_eq!(pixel(&output, 1, y), pixel(&to, 2, y));
        assert_eq!(pixel(&output, 2, y), pixel(&from, 2, y));
        assert_eq!(pixel(&output, 3, y), pixel(&from, 3, y));
        assert_eq!(pixel(&output, 4, y), pixel(&to, 3, y));
        assert_eq!(pixel(&output, 6, y), pixel(&to, 5, y));
    }
}

#[test]
fn fly_draws_a_scaled_preview_over_static_program() {
    let from = pattern(9, 7, 0, 0);
    let color = Rgba8::new(40, 80, 120, 255);
    let to = solid(9, 7, color);
    // p = 1/2: scale 3/4, centre 4.5 pixels right of the frame centre.
    let output = run(TransitionKind::Fly, &from, &to, 1, 2);
    for y in 0..7 {
        for x in 0..5 {
            assert_eq!(pixel(&output, x, y), pixel(&from, x, y), "({x}, {y})");
        }
    }
    for (x, y) in [(6, 3), (7, 1), (8, 5)] {
        assert_eq!(pixel(&output, x, y), color, "({x}, {y})");
    }
    // Column 5 maps to texel -2/3: Q8 -171, one transparent tap at weight
    // 171/256 and the colour at 85/256.
    let edge = Rgba8::new(13, 27, 40, 85);
    assert_eq!(pixel(&output, 5, 3), over(edge, pixel(&from, 5, 3)));
}

#[test]
fn fly_rotate_turns_preview_clockwise() {
    let from = pattern(9, 7, 0, 0);
    let (blue, green) = (Rgba8::new(0, 0, 200, 255), Rgba8::new(0, 200, 0, 255));
    let to = columns(9, 7, 4, blue, green);
    // p = 1/2: a quarter turn clockwise (exact), so Preview's left columns
    // are on top.
    let output = run(TransitionKind::FlyRotate, &from, &to, 1, 2);
    assert_eq!(pixel(&output, 8, 0), blue);
    assert_eq!(pixel(&output, 8, 6), green);
    assert_eq!(pixel(&output, 0, 3), pixel(&from, 0, 3));
}

#[test]
fn cube_turns_program_away_and_preview_in() {
    let red = Rgba8::new(200, 0, 0, 255);
    let (blue, green) = (Rgba8::new(0, 0, 200, 255), Rgba8::new(0, 200, 0, 255));
    let from = solid(31, 15, red);
    let to = columns(31, 15, 15, blue, green);
    let transparent = Rgba8::new(0, 0, 0, 0);

    // A turn below one millidegree is the untouched Program.
    let (program, preview) = (pattern(9, 7, 0, 0), pattern(9, 7, 1, 0));
    assert_eq!(
        run(TransitionKind::Cube, &program, &preview, 1, 100_000),
        program
    );

    // 45 degrees: Program on the left, Preview's left half next to the edge.
    let output = run(TransitionKind::Cube, &from, &to, 1, 2);
    assert_eq!(pixel(&output, 2, 7), red);
    assert_eq!(pixel(&output, 14, 7), red);
    assert_eq!(pixel(&output, 16, 7), blue);
    assert_eq!(pixel(&output, 29, 7), green);
    assert_eq!(pixel(&output, 0, 0), transparent);
    assert_eq!(pixel(&output, 30, 14), transparent);
    // The crease is a hard edge between the two opaque faces, with no
    // translucent seam at any angle: every row is opaque between its first
    // and last opaque pixel.
    assert_eq!(pixel(&output, 15, 7), blue);
    for (n, d) in [(1, 5), (1, 3), (1, 2), (2, 3), (4, 5)] {
        let output = run(TransitionKind::Cube, &from, &to, n, d);
        for y in 3..12 {
            let opaque = (0..31)
                .filter(|&x| pixel(&output, x, y).a == 255)
                .collect::<Vec<_>>();
            let (first, last) = (opaque[0], opaque[opaque.len() - 1]);
            for x in first..=last {
                assert!(
                    pixel(&output, x, y).a == 255,
                    "{n}/{d} ({x}, {y}): {:?}",
                    pixel(&output, x, y)
                );
            }
        }
    }

    // 67.5 degrees: Program is a narrow strip and the background shows left.
    let output = run(TransitionKind::Cube, &from, &to, 3, 4);
    assert_eq!(pixel(&output, 0, 7), transparent);
    assert_eq!(pixel(&output, 3, 7), red);
    assert_eq!(pixel(&output, 10, 7), blue);
    assert_eq!(pixel(&output, 25, 7), green);

    // CubeZoom recedes by a further W at the midpoint: transparent margins.
    let output = run(TransitionKind::CubeZoom, &from, &to, 1, 2);
    for (x, y) in [(0, 0), (1, 7), (29, 7), (15, 0)] {
        assert_eq!(pixel(&output, x, y), transparent, "({x}, {y})");
    }
    assert_eq!(pixel(&output, 5, 7), red);
    assert_eq!(pixel(&output, 17, 7), blue);
    assert_eq!(pixel(&output, 25, 7), green);
}

#[test]
fn cross_zoom_magnifies_both_and_crossfades() {
    let (from, to) = (pattern(9, 7, 0, 0), pattern(9, 7, 1, 0));
    // The centre texel of an odd frame is a fixed point of both zooms.
    for (n, d) in [(1, 4), (1, 2), (5, 7)] {
        let output = run(TransitionKind::CrossZoom, &from, &to, n, d);
        let (first, second) = (pixel(&from, 4, 3), pixel(&to, 4, 3));
        assert_eq!(
            pixel(&output, 4, 3),
            Rgba8::new(
                blend(first.r, second.r, n, d),
                blend(first.g, second.g, n, d),
                blend(first.b, second.b, n, d),
                blend(first.a, second.a, n, d),
            )
        );
    }
    // Magnified pictures never sample outside: solid inputs stay solid, with
    // no transparent fringe.
    let (left, right) = (Rgba8::new(200, 0, 0, 255), Rgba8::new(0, 100, 50, 255));
    let output = run(
        TransitionKind::CrossZoom,
        &solid(9, 7, left),
        &solid(9, 7, right),
        1,
        3,
    );
    let expected = Rgba8::new(133, 33, 17, 255);
    for y in 0..7 {
        assert_eq!(row(&output, y), vec![expected; 9]);
    }
}

#[test]
fn checksums_pin_the_reference_arithmetic() {
    let (from, to) = (pattern(13, 9, 0, 0), pattern(13, 9, 1, 0));
    let mut actual = Vec::new();
    for kind in NEW_KINDS {
        for (n, d) in [(1, 3), (1, 2), (4, 5)] {
            actual.push((kind, n, d, fnv1a(&run(kind, &from, &to, n, d))));
        }
    }
    let expected: &[(TransitionKind, u32, u32, u64)] = &[
        (
            TransitionKind::VerticalWipe,
            1,
            3,
            5_601_216_867_723_557_615,
        ),
        (
            TransitionKind::VerticalWipe,
            1,
            2,
            16_890_130_748_537_032_829,
        ),
        (
            TransitionKind::VerticalWipe,
            4,
            5,
            10_831_711_489_572_322_219,
        ),
        (
            TransitionKind::VerticalSlide,
            1,
            3,
            2_771_782_158_341_309_711,
        ),
        (
            TransitionKind::VerticalSlide,
            1,
            2,
            16_065_475_663_876_098_389,
        ),
        (
            TransitionKind::VerticalSlide,
            4,
            5,
            3_120_183_633_153_628_659,
        ),
        (TransitionKind::Fly, 1, 3, 12_530_834_728_358_671_125),
        (TransitionKind::Fly, 1, 2, 17_596_518_124_524_281_976),
        (TransitionKind::Fly, 4, 5, 3_257_517_393_742_182_985),
        (TransitionKind::FlyRotate, 1, 3, 924_362_470_327_791_927),
        (TransitionKind::FlyRotate, 1, 2, 8_403_051_459_490_619_155),
        (TransitionKind::FlyRotate, 4, 5, 16_289_669_763_723_641_714),
        (TransitionKind::Cube, 1, 3, 6_192_777_892_665_715_496),
        (TransitionKind::Cube, 1, 2, 18_279_526_969_852_658_489),
        (TransitionKind::Cube, 4, 5, 5_268_596_748_479_606_805),
        (TransitionKind::CubeZoom, 1, 3, 7_939_608_549_303_290_589),
        (TransitionKind::CubeZoom, 1, 2, 18_228_138_146_412_409_332),
        (TransitionKind::CubeZoom, 4, 5, 14_892_352_177_200_062_822),
        (TransitionKind::CrossZoom, 1, 3, 10_150_074_337_679_053_733),
        (TransitionKind::CrossZoom, 1, 2, 9_329_186_384_993_459_004),
        (TransitionKind::CrossZoom, 4, 5, 17_325_950_226_723_421_586),
        (TransitionKind::Merge, 1, 3, 12_148_737_104_932_280_263),
        (TransitionKind::Merge, 1, 2, 6_488_482_702_320_715_128),
        (TransitionKind::Merge, 4, 5, 7_075_873_460_115_814_799),
    ];
    for (actual, expected) in actual.iter().zip(expected) {
        assert_eq!(actual, expected);
    }
    assert_eq!(actual.len(), expected.len());
}

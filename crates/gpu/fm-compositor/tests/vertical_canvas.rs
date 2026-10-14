//! SW-015 vertical and arbitrary canvas production on the CPU reference path.
//!
//! Every expectation is exact integer geometry: portrait, square, and odd
//! canvases; fit/fill/stretch placement of differently shaped sources; the
//! vMix-style 1080x1920 vertical crop, safe-area, and rotation workflow; masks
//! after a fill crop; and aspect-preserving multiview cells.

use core::num::NonZeroU128;

use fm_compositor::{
    CanvasRect, ConfidenceMode, CpuSourceFrame, CropRect, FitError, FitPolicy, GridLayout,
    MultiviewDefinition, MultiviewError, MultiviewOverlays, MultiviewSource, MultiviewSourceFrame,
    OutputTarget, RectMask, Rgba8, Rotation, SafeAreaError, SafeAreaGuide, SafeAreaPreset, Scene,
    SceneError, SourceId, SourcePlacement, Transform, center_cut, compile_multiview_scene_fitted,
    compile_scene, execute_cpu, place_source, plan_multiview, preset_guides, push_preset_guides,
};
use fm_types::InputId;
use fm_video::ImageFrame;

const BACKGROUND: Rgba8 = Rgba8::new(0, 0, 0, 255);
const YELLOW: Rgba8 = Rgba8::new(255, 255, 0, 255);
const CAMERA: SourceId = SourceId::new(1);

/// A source whose every pixel encodes its own coordinates: `r = x`, `g = y`.
fn coordinate_frame(width: u32, height: u32) -> ImageFrame {
    assert!(width <= 256 && height <= 256);
    let bytes = (0..height)
        .flat_map(|y| {
            (0..width).flat_map(move |x| {
                Rgba8::new(u8::try_from(x).unwrap(), u8::try_from(y).unwrap(), 128, 255).to_bytes()
            })
        })
        .collect();
    ImageFrame::new(width, height, usize::try_from(width).unwrap() * 4, bytes).unwrap()
}

fn solid_frame(width: u32, height: u32, color: Rgba8) -> ImageFrame {
    let bytes = (0..width * height).flat_map(|_| color.to_bytes()).collect();
    ImageFrame::new(width, height, usize::try_from(width).unwrap() * 4, bytes).unwrap()
}

fn coordinate(x: u32, y: u32) -> Rgba8 {
    Rgba8::new(u8::try_from(x).unwrap(), u8::try_from(y).unwrap(), 128, 255)
}

fn render(
    width: u32,
    height: u32,
    layer: fm_compositor::SourceLayer,
    source: &ImageFrame,
) -> ImageFrame {
    let mut scene = Scene::new(width, height, BACKGROUND).unwrap();
    scene.push_layer(layer);
    let (plan, _) = compile_scene(&scene, OutputTarget::Program).unwrap();
    execute_cpu(&plan, &[CpuSourceFrame::new(CAMERA, source)]).unwrap()
}

/// The documented sampling rule: a canvas pixel inside the rotated bounds maps
/// back through the quarter turn, then nearest-neighbor into the crop.
fn expected_sample(placement: &SourcePlacement, source: (u32, u32), x: u32, y: u32) -> Rgba8 {
    let (bx, by, bw, bh) = placement.bounds();
    let (x, y) = (i64::from(x), i64::from(y));
    let (bx, by) = (i64::from(bx), i64::from(by));
    if x < bx || y < by || x >= bx + i64::from(bw) || y >= by + i64::from(bh) {
        return BACKGROUND;
    }
    let rx = u32::try_from(x - bx).unwrap();
    let ry = u32::try_from(y - by).unwrap();
    let Transform {
        scale_width,
        scale_height,
        rotation,
        ..
    } = placement.transform();
    let (sx, sy) = match rotation {
        Rotation::Deg0 => (rx, ry),
        Rotation::Deg90 => (ry, scale_height - 1 - rx),
        Rotation::Deg180 => (scale_width - 1 - rx, scale_height - 1 - ry),
        Rotation::Deg270 => (scale_width - 1 - ry, rx),
    };
    let crop = placement
        .crop()
        .unwrap_or(CropRect::new(0, 0, source.0, source.1));
    coordinate(
        crop.x + sx * crop.width / scale_width,
        crop.y + sy * crop.height / scale_height,
    )
}

fn assert_matches_placement(output: &ImageFrame, placement: &SourcePlacement, source: (u32, u32)) {
    for y in 0..output.height() {
        for x in 0..output.width() {
            assert_eq!(
                output.pixel(x, y).unwrap(),
                expected_sample(placement, source, x, y),
                "pixel ({x}, {y})"
            );
        }
    }
}

#[test]
fn scenes_accept_portrait_square_odd_and_extreme_canvases() {
    for (width, height) in [
        (1080, 1920),
        (1080, 1080),
        (1920, 1080),
        (720, 1280),
        (2160, 3840),
        (107, 191),
        (1, 16_384),
        (16_384, 1),
    ] {
        let scene = Scene::new(width, height, BACKGROUND).unwrap();
        assert_eq!((scene.width(), scene.height()), (width, height));
    }
    assert_eq!(
        Scene::new(1080, 16_385, BACKGROUND),
        Err(SceneError::DimensionsTooLarge {
            width: 1080,
            height: 16_385
        })
    );
}

#[test]
fn center_cut_is_exact_for_every_orientation() {
    assert_eq!(
        center_cut(1920, 1080, 9, 16),
        Ok(CropRect::new(656, 0, 608, 1080))
    );
    assert_eq!(
        center_cut(1080, 1920, 16, 9),
        Ok(CropRect::new(0, 656, 1080, 608))
    );
    assert_eq!(
        center_cut(1080, 1080, 16, 9),
        Ok(CropRect::new(0, 236, 1080, 608))
    );
    assert_eq!(
        center_cut(1080, 1080, 9, 16),
        Ok(CropRect::new(236, 0, 608, 1080))
    );
    assert_eq!(
        center_cut(1080, 1920, 9, 16),
        Ok(CropRect::new(0, 0, 1080, 1920))
    );
    assert_eq!(
        center_cut(1080, 1080, 1, 1),
        Ok(CropRect::new(0, 0, 1080, 1080))
    );
    // 1081 * 9 / 16 = 608.06 rounds to 608; the odd leftover goes to the bottom.
    assert_eq!(
        center_cut(1081, 1921, 16, 9),
        Ok(CropRect::new(0, 656, 1081, 608))
    );
    // A sliver never collapses to zero.
    assert_eq!(center_cut(1, 1000, 1, 1), Ok(CropRect::new(0, 499, 1, 1)));
    assert_eq!(
        center_cut(1000, 1, 1, 10_000),
        Ok(CropRect::new(499, 0, 1, 1))
    );
    assert_eq!(center_cut(0, 1, 1, 1), Err(FitError::ZeroDimension));
    assert_eq!(center_cut(1, 1, 0, 1), Err(FitError::ZeroDimension));
    // Extreme terms stay exact without overflow.
    assert_eq!(
        center_cut(u32::MAX, u32::MAX, u32::MAX, 1),
        Ok(CropRect::new(0, u32::MAX / 2, u32::MAX, 1))
    );
}

#[test]
fn landscape_source_on_a_1080x1920_canvas_follows_each_policy() {
    let canvas = CanvasRect::full(1080, 1920);
    let fit = place_source(1920, 1080, None, canvas, Rotation::Deg0, FitPolicy::Fit).unwrap();
    assert_eq!(fit.crop(), None);
    assert_eq!(
        fit.transform(),
        Transform::new(0, 656, 1080, 608, Rotation::Deg0)
    );
    assert_eq!(fit.bounds(), (0, 656, 1080, 608));

    let fill = place_source(1920, 1080, None, canvas, Rotation::Deg0, FitPolicy::Fill).unwrap();
    assert_eq!(fill.crop(), Some(CropRect::new(656, 0, 608, 1080)));
    assert_eq!(
        fill.transform(),
        Transform::new(0, 0, 1080, 1920, Rotation::Deg0)
    );

    let stretch =
        place_source(1920, 1080, None, canvas, Rotation::Deg0, FitPolicy::Stretch).unwrap();
    assert_eq!(stretch.crop(), None);
    assert_eq!(
        stretch.transform(),
        Transform::new(0, 0, 1080, 1920, Rotation::Deg0)
    );

    // A camera mounted sideways: a quarter turn fills the portrait canvas with
    // no crop and no bars under every policy.
    for policy in [FitPolicy::Fit, FitPolicy::Fill, FitPolicy::Stretch] {
        for rotation in [Rotation::Deg90, Rotation::Deg270] {
            let turned = place_source(1920, 1080, None, canvas, rotation, policy).unwrap();
            assert_eq!(turned.crop(), None);
            assert_eq!(
                turned.transform(),
                Transform::new(0, 0, 1920, 1080, rotation)
            );
            assert_eq!(turned.bounds(), (0, 0, 1080, 1920));
        }
    }
}

#[test]
fn square_and_landscape_canvases_place_foreign_aspects_exactly() {
    let square = CanvasRect::full(1080, 1080);
    let fit = place_source(1920, 1080, None, square, Rotation::Deg0, FitPolicy::Fit).unwrap();
    assert_eq!(fit.bounds(), (0, 236, 1080, 608));
    let fill = place_source(1920, 1080, None, square, Rotation::Deg0, FitPolicy::Fill).unwrap();
    assert_eq!(fill.crop(), Some(CropRect::new(420, 0, 1080, 1080)));
    assert_eq!(fill.bounds(), (0, 0, 1080, 1080));
    let portrait = place_source(1080, 1920, None, square, Rotation::Deg0, FitPolicy::Fit).unwrap();
    assert_eq!(portrait.bounds(), (236, 0, 608, 1080));

    // A vertical phone source pillarboxed on a landscape program.
    let landscape = CanvasRect::full(1920, 1080);
    let phone = place_source(1080, 1920, None, landscape, Rotation::Deg0, FitPolicy::Fit).unwrap();
    assert_eq!(phone.crop(), None);
    assert_eq!(
        phone.transform(),
        Transform::new(656, 0, 608, 1080, Rotation::Deg0)
    );
    let phone_fill =
        place_source(1080, 1920, None, landscape, Rotation::Deg0, FitPolicy::Fill).unwrap();
    assert_eq!(phone_fill.crop(), Some(CropRect::new(0, 656, 1080, 608)));

    // A sub-rectangle target offsets the picture; odd leftovers go right/bottom.
    let cell = CanvasRect::new(100, 50, 101, 61);
    let boxed = place_source(20, 20, None, cell, Rotation::Deg0, FitPolicy::Fit).unwrap();
    assert_eq!(boxed.bounds(), (120, 50, 61, 61));
    let rotated = place_source(40, 20, None, cell, Rotation::Deg90, FitPolicy::Fit).unwrap();
    // Turned target is 61x101; a 2:1 source fits as 61x31 (30.5 rounds up),
    // which rotates to a 31x61 footprint.
    assert_eq!(
        rotated.transform(),
        Transform::new(135, 50, 61, 31, Rotation::Deg90)
    );
    assert_eq!(rotated.bounds(), (135, 50, 31, 61));
}

#[test]
fn vmix_vertical_crop_workflow_matches_its_operator_guide() {
    // The operator outlines the 9:16 cut on the landscape camera's view...
    let guides = preset_guides(
        1920,
        1080,
        SafeAreaPreset::CenterCut {
            aspect_width: 9,
            aspect_height: 16,
        },
        YELLOW,
    )
    .unwrap();
    assert_eq!(guides, vec![SafeAreaGuide::new(656, 0, 608, 1080, YELLOW)]);

    // ...and the portrait Fill placement keeps exactly that region.
    let canvas = CanvasRect::full(1080, 1920);
    let fill = place_source(1920, 1080, None, canvas, Rotation::Deg0, FitPolicy::Fill).unwrap();
    let crop = fill.crop().unwrap();
    assert_eq!(
        (crop.x, crop.y, crop.width, crop.height),
        (guides[0].x, guides[0].y, guides[0].width, guides[0].height)
    );

    // An authored crop of that strip, re-placed by Fill, is used unchanged.
    let strip = Some(crop);
    let refill = place_source(1920, 1080, strip, canvas, Rotation::Deg0, FitPolicy::Fill).unwrap();
    assert_eq!(refill, fill);
    // Fit keeps the whole 608x1080 strip: 1918 rows high (1918.4 rounds down),
    // with the single leftover row split as 1 above and 1 below.
    let refit = place_source(1920, 1080, strip, canvas, Rotation::Deg0, FitPolicy::Fit).unwrap();
    assert_eq!(refit.crop(), strip);
    assert_eq!(refit.bounds(), (0, 1, 1080, 1918));
}

#[test]
fn placement_rejects_invalid_requests() {
    let canvas = CanvasRect::full(1080, 1920);
    let place = |width, height, region, target| {
        place_source(
            width,
            height,
            region,
            target,
            Rotation::Deg0,
            FitPolicy::Fit,
        )
    };
    assert_eq!(place(0, 1080, None, canvas), Err(FitError::ZeroDimension));
    assert_eq!(
        place(1920, 1080, None, CanvasRect::full(1080, 0)),
        Err(FitError::ZeroDimension)
    );
    assert_eq!(
        place(16_385, 1080, None, canvas),
        Err(FitError::SourceTooLarge {
            width: 16_385,
            height: 1080
        })
    );
    let outside = CropRect::new(1900, 0, 21, 1080);
    assert_eq!(
        place(1920, 1080, Some(outside), canvas),
        Err(FitError::RegionOutsideSource {
            region: outside,
            source_width: 1920,
            source_height: 1080
        })
    );
    let empty = CropRect::new(0, 0, 0, 1080);
    assert!(matches!(
        place(1920, 1080, Some(empty), canvas),
        Err(FitError::RegionOutsideSource { .. })
    ));
    let overflow = CropRect::new(u32::MAX, 0, 1, 1);
    assert!(matches!(
        place(1920, 1080, Some(overflow), canvas),
        Err(FitError::RegionOutsideSource { .. })
    ));
    let huge = CanvasRect::full(1080, 16_385);
    assert_eq!(
        place(1920, 1080, None, huge),
        Err(FitError::TargetTooLarge { target: huge })
    );
    let far = CanvasRect::new(u32::try_from(i32::MAX).unwrap(), 0, 1, 1);
    assert_eq!(
        place(1920, 1080, None, far),
        Err(FitError::TargetOutOfRange { target: far })
    );
}

#[test]
fn portrait_fit_render_letterboxes_with_exact_samples() {
    let source = coordinate_frame(192, 108);
    let placement = place_source(
        192,
        108,
        None,
        CanvasRect::full(108, 192),
        Rotation::Deg0,
        FitPolicy::Fit,
    )
    .unwrap();
    // 108 * 108 / 192 = 60.75 rounds to 61 rows, centered at row 65.
    assert_eq!(placement.bounds(), (0, 65, 108, 61));
    let output = render(108, 192, placement.layer(CAMERA, 0), &source);
    assert_eq!((output.width(), output.height()), (108, 192));
    assert_eq!(output.pixel(0, 64), Some(BACKGROUND));
    assert_eq!(output.pixel(0, 65), Some(coordinate(0, 0)));
    assert_eq!(output.pixel(107, 125), Some(coordinate(190, 106)));
    assert_eq!(output.pixel(107, 126), Some(BACKGROUND));
    assert_matches_placement(&output, &placement, (192, 108));
}

#[test]
fn portrait_fill_render_center_crops_with_exact_samples() {
    let source = coordinate_frame(192, 108);
    let placement = place_source(
        192,
        108,
        None,
        CanvasRect::full(108, 192),
        Rotation::Deg0,
        FitPolicy::Fill,
    )
    .unwrap();
    assert_eq!(placement.crop(), Some(CropRect::new(65, 0, 61, 108)));
    let output = render(108, 192, placement.layer(CAMERA, 0), &source);
    assert_eq!(output.pixel(0, 0), Some(coordinate(65, 0)));
    assert_eq!(output.pixel(107, 191), Some(coordinate(125, 107)));
    assert_matches_placement(&output, &placement, (192, 108));
}

#[test]
fn portrait_rotation_render_turns_the_landscape_source_clockwise() {
    let source = coordinate_frame(192, 108);
    for rotation in [Rotation::Deg90, Rotation::Deg270] {
        let placement = place_source(
            192,
            108,
            None,
            CanvasRect::full(108, 192),
            rotation,
            FitPolicy::Fit,
        )
        .unwrap();
        let output = render(108, 192, placement.layer(CAMERA, 0), &source);
        assert_matches_placement(&output, &placement, (192, 108));
    }
    let clockwise = place_source(
        192,
        108,
        None,
        CanvasRect::full(108, 192),
        Rotation::Deg90,
        FitPolicy::Fit,
    )
    .unwrap();
    let output = render(108, 192, clockwise.layer(CAMERA, 0), &source);
    // A clockwise quarter turn brings the source's bottom-left to the top-left
    // and its top-left to the top-right.
    assert_eq!(output.pixel(0, 0), Some(coordinate(0, 107)));
    assert_eq!(output.pixel(107, 0), Some(coordinate(0, 0)));
    assert_eq!(output.pixel(107, 191), Some(coordinate(191, 0)));
}

#[test]
fn square_and_odd_canvases_render_exactly() {
    let source = coordinate_frame(192, 108);
    for (width, height, policy) in [
        (120, 120, FitPolicy::Fit),
        (120, 120, FitPolicy::Fill),
        (107, 191, FitPolicy::Fit),
        (107, 191, FitPolicy::Fill),
        (107, 191, FitPolicy::Stretch),
    ] {
        let placement = place_source(
            192,
            108,
            None,
            CanvasRect::full(width, height),
            Rotation::Deg0,
            policy,
        )
        .unwrap();
        let output = render(width, height, placement.layer(CAMERA, 0), &source);
        assert_matches_placement(&output, &placement, (192, 108));
    }
    let square = place_source(
        192,
        108,
        None,
        CanvasRect::full(120, 120),
        Rotation::Deg0,
        FitPolicy::Fit,
    )
    .unwrap();
    // 120 * 108 / 192 = 67.5 rounds up to 68 rows at row 26.
    assert_eq!(square.bounds(), (0, 26, 120, 68));
}

#[test]
fn masks_apply_in_post_crop_space_on_a_portrait_fill() {
    let source = coordinate_frame(192, 108);
    let placement = place_source(
        192,
        108,
        None,
        CanvasRect::full(108, 192),
        Rotation::Deg0,
        FitPolicy::Fill,
    )
    .unwrap();
    // Keep the left 30 of the 61 cropped columns: canvas column X samples crop
    // column X * 61 / 108, so columns 0..=53 stay and 54.. are masked.
    let mask = RectMask::new(0, 0, 30, 108);
    let kept = render(
        108,
        192,
        placement.layer(CAMERA, 0).with_mask(mask),
        &source,
    );
    let cut = render(
        108,
        192,
        placement.layer(CAMERA, 0).with_mask(mask.inverted(true)),
        &source,
    );
    for y in 0..192 {
        for x in 0..108 {
            let sample = expected_sample(&placement, (192, 108), x, y);
            let inside = x * 61 / 108 < 30;
            assert_eq!(x <= 53, inside);
            let (kept_expected, cut_expected) = if inside {
                (sample, BACKGROUND)
            } else {
                (BACKGROUND, sample)
            };
            assert_eq!(kept.pixel(x, y), Some(kept_expected), "kept ({x}, {y})");
            assert_eq!(cut.pixel(x, y), Some(cut_expected), "cut ({x}, {y})");
        }
    }
}

#[test]
fn portrait_and_square_safe_areas_are_exact_and_operator_only() {
    let smpte = preset_guides(1080, 1920, SafeAreaPreset::Smpte, YELLOW).unwrap();
    assert_eq!(
        smpte,
        vec![
            SafeAreaGuide::new(38, 67, 1004, 1785, YELLOW),
            SafeAreaGuide::new(54, 96, 972, 1728, YELLOW),
        ]
    );
    let square = preset_guides(1080, 1080, SafeAreaPreset::Smpte, YELLOW).unwrap();
    assert_eq!(
        square,
        vec![
            SafeAreaGuide::new(38, 38, 1004, 1004, YELLOW),
            SafeAreaGuide::new(54, 54, 972, 972, YELLOW),
        ]
    );
    // A 16:9 band on the portrait canvas shows where a landscape source fits.
    let band = preset_guides(
        1080,
        1920,
        SafeAreaPreset::CenterCut {
            aspect_width: 16,
            aspect_height: 9,
        },
        YELLOW,
    )
    .unwrap();
    let fit = place_source(
        1920,
        1080,
        None,
        CanvasRect::full(1080, 1920),
        Rotation::Deg0,
        FitPolicy::Fit,
    )
    .unwrap();
    let (x, y, width, height) = fit.bounds();
    assert_eq!(
        band,
        vec![SafeAreaGuide::new(
            u32::try_from(x).unwrap(),
            u32::try_from(y).unwrap(),
            width,
            height,
            YELLOW
        )]
    );
    assert_eq!(
        preset_guides(
            1080,
            1920,
            SafeAreaPreset::CenterCut {
                aspect_width: 0,
                aspect_height: 9
            },
            YELLOW
        ),
        Err(SafeAreaError::ZeroAspect {
            aspect_width: 0,
            aspect_height: 9
        })
    );

    // Rendered on a small portrait operator canvas, the guide outlines land on
    // exact pixels and every other pixel is the source.
    let red = Rgba8::new(255, 0, 0, 255);
    let source = solid_frame(108, 192, red);
    let mut scene = Scene::new(108, 192, BACKGROUND).unwrap();
    scene.push_layer(fm_compositor::SourceLayer::new(
        CAMERA,
        0,
        Transform::new(0, 0, 108, 192, Rotation::Deg0),
    ));
    assert_eq!(
        push_preset_guides(&mut scene, SafeAreaPreset::Smpte, YELLOW),
        Ok(2)
    );
    // Action 93%: 100x178 at (4, 7). Title 90%: 97x172 at (5, 10).
    assert_eq!(
        scene.safe_areas(),
        &[
            SafeAreaGuide::new(4, 7, 100, 178, YELLOW),
            SafeAreaGuide::new(5, 10, 97, 172, YELLOW),
        ]
    );
    let on_outline = |guide: &SafeAreaGuide, x: u32, y: u32| {
        let right = guide.x + guide.width - 1;
        let bottom = guide.y + guide.height - 1;
        let inside_x = (guide.x..=right).contains(&x);
        let inside_y = (guide.y..=bottom).contains(&y);
        (inside_x && (y == guide.y || y == bottom)) || (inside_y && (x == guide.x || x == right))
    };
    let (operator, _) = compile_scene(&scene, OutputTarget::Operator).unwrap();
    let output = execute_cpu(&operator, &[CpuSourceFrame::new(CAMERA, &source)]).unwrap();
    for y in 0..192 {
        for x in 0..108 {
            let outlined = scene.safe_areas().iter().any(|g| on_outline(g, x, y));
            let expected = if outlined { YELLOW } else { red };
            assert_eq!(output.pixel(x, y), Some(expected), "operator ({x}, {y})");
        }
    }
    let (program, _) = compile_scene(&scene, OutputTarget::Program).unwrap();
    let clean = execute_cpu(&program, &[CpuSourceFrame::new(CAMERA, &source)]).unwrap();
    assert_eq!(clean, source);
}

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

#[test]
fn landscape_multiview_keeps_a_portrait_program_aspect() {
    let definition = MultiviewDefinition::new(
        "vertical show",
        GridLayout::two_by_two(),
        vec![MultiviewSource::Program, MultiviewSource::Input(input(1))],
        ConfidenceMode::None,
        MultiviewOverlays::none(),
    )
    .unwrap();
    let plan = plan_multiview(192, 108, &definition).unwrap();
    let program = SourceId::new(10);
    let camera = SourceId::new(11);
    let resolve = |source| match source {
        MultiviewSource::Program => Some(MultiviewSourceFrame::new(program, 54, 96)),
        MultiviewSource::Input(_) => Some(MultiviewSourceFrame::new(camera, 192, 108)),
        MultiviewSource::Preview => None,
    };
    let scene =
        compile_multiview_scene_fitted(&plan, 192, 108, BACKGROUND, 0, FitPolicy::Fit, resolve)
            .unwrap();
    // The 54x96 portrait Program pillarboxes inside its 96x54 cell:
    // 54 * 54 / 96 = 30.375 rounds to 30 columns centered at x = 33.
    assert_eq!(
        scene.layers()[0].transform(),
        Transform::new(33, 0, 30, 54, Rotation::Deg0)
    );
    assert_eq!(scene.layers()[0].crop(), None);
    // The landscape camera already matches its cell.
    assert_eq!(
        scene.layers()[1].transform(),
        Transform::new(96, 0, 96, 54, Rotation::Deg0)
    );

    let green = Rgba8::new(0, 255, 0, 255);
    let blue = Rgba8::new(0, 0, 255, 255);
    let program_frame = solid_frame(54, 96, green);
    let camera_frame = solid_frame(192, 108, blue);
    let (compiled, _) = compile_scene(&scene, OutputTarget::Program).unwrap();
    let output = execute_cpu(
        &compiled,
        &[
            CpuSourceFrame::new(program, &program_frame),
            CpuSourceFrame::new(camera, &camera_frame),
        ],
    )
    .unwrap();
    for y in 0..108 {
        for x in 0..192 {
            let expected = if y < 54 && (33..63).contains(&x) {
                green
            } else if y < 54 && x >= 96 {
                blue
            } else {
                BACKGROUND
            };
            assert_eq!(output.pixel(x, y), Some(expected), "multiview ({x}, {y})");
        }
    }

    // Fill crops the portrait Program to the cell shape instead.
    let filled =
        compile_multiview_scene_fitted(&plan, 192, 108, BACKGROUND, 0, FitPolicy::Fill, resolve)
            .unwrap();
    // 54 * 54 / 96 = 30.375 rounds to 30 source rows centered at row 33.
    assert_eq!(
        filled.layers()[0].crop(),
        Some(CropRect::new(0, 33, 54, 30))
    );
    assert_eq!(
        filled.layers()[0].transform(),
        Transform::new(0, 0, 96, 54, Rotation::Deg0)
    );

    let unresolved =
        compile_multiview_scene_fitted(&plan, 192, 108, BACKGROUND, 0, FitPolicy::Fit, |source| {
            match source {
                MultiviewSource::Program => Some(MultiviewSourceFrame::new(program, 0, 96)),
                _ => resolve(source),
            }
        });
    assert_eq!(
        unresolved,
        Err(MultiviewError::Placement {
            cell: 0,
            error: FitError::ZeroDimension
        })
    );
}

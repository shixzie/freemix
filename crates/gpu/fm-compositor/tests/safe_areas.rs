use fm_compositor::{
    CompositionPlan, MAX_SAFE_AREA_GUIDES, OutputTarget, ReportEntry, Rgba8, SafeAreaError,
    SafeAreaGuide, SafeAreaPreset, Scene, compile_scene, execute_cpu, preset_guides,
    push_preset_guides,
};

const BACKGROUND: Rgba8 = Rgba8::new(0, 0, 0, 255);
const YELLOW: Rgba8 = Rgba8::new(255, 255, 0, 255);
const MAGENTA: Rgba8 = Rgba8::new(255, 0, 255, 255);

fn guide(x: u32, y: u32, width: u32, height: u32, color: Rgba8) -> SafeAreaGuide {
    SafeAreaGuide::new(x, y, width, height, color)
}

fn custom(x: u16, y: u16, width: u16, height: u16) -> SafeAreaPreset {
    SafeAreaPreset::Custom {
        x_basis_points: x,
        y_basis_points: y,
        width_basis_points: width,
        height_basis_points: height,
    }
}

fn out_of_bounds(
    x: u16,
    y: u16,
    width: u16,
    height: u16,
    canvas_width: u32,
    canvas_height: u32,
) -> SafeAreaError {
    SafeAreaError::CustomGuideOutOfBounds {
        x_basis_points: x,
        y_basis_points: y,
        width_basis_points: width,
        height_basis_points: height,
        canvas_width,
        canvas_height,
    }
}

fn contains(outer: SafeAreaGuide, inner: SafeAreaGuide) -> bool {
    inner.x >= outer.x
        && inner.y >= outer.y
        && inner.x + inner.width <= outer.x + outer.width
        && inner.y + inner.height <= outer.y + outer.height
}

#[test]
fn smpte_fixed_presets_are_exact_centered_rectangles() {
    // 1920 * 93 / 100 = 1785 and 1080 * 93 / 100 = 1004, centered at
    // ((1920 - 1785) / 2, (1080 - 1004) / 2) = (67, 38).
    let action = preset_guides(1920, 1080, SafeAreaPreset::Action, YELLOW).unwrap();
    assert_eq!(action, vec![guide(67, 38, 1785, 1004, YELLOW)]);

    // 1920 * 90 / 100 = 1728 and 1080 * 90 / 100 = 972, centered at (96, 54).
    let title = preset_guides(1920, 1080, SafeAreaPreset::Title, YELLOW).unwrap();
    assert_eq!(title, vec![guide(96, 54, 1728, 972, YELLOW)]);

    let smpte = preset_guides(1920, 1080, SafeAreaPreset::Smpte, YELLOW).unwrap();
    assert_eq!(
        smpte,
        vec![
            guide(67, 38, 1785, 1004, YELLOW),
            guide(96, 54, 1728, 972, YELLOW),
        ]
    );
    assert!(
        contains(smpte[0], smpte[1]),
        "action safe must nest title safe"
    );
    assert!(
        !contains(smpte[1], smpte[0]),
        "title safe must not nest action safe"
    );
    assert_eq!(
        smpte,
        [
            preset_guides(1920, 1080, SafeAreaPreset::Action, YELLOW).unwrap(),
            preset_guides(1920, 1080, SafeAreaPreset::Title, YELLOW).unwrap(),
        ]
        .concat()
    );
}

#[test]
fn odd_canvases_center_with_truncating_division() {
    // 1919 * 93 / 100 = 1784.67 -> 1784 and 1079 * 93 / 100 = 1003.47 -> 1003.
    let action = preset_guides(1919, 1079, SafeAreaPreset::Action, YELLOW).unwrap();
    assert_eq!(action, vec![guide(67, 38, 1784, 1003, YELLOW)]);

    // 1919 * 90 / 100 = 1727.1 -> 1727 and 1079 * 90 / 100 = 971.1 -> 971.
    let title = preset_guides(1919, 1079, SafeAreaPreset::Title, YELLOW).unwrap();
    assert_eq!(title, vec![guide(96, 54, 1727, 971, YELLOW)]);

    // 101 * 93 / 100 = 93.93 -> 93 (rounding would give 94) and
    // 101 * 90 / 100 = 90.9 -> 90 (rounding would give 91).
    let action = preset_guides(101, 101, SafeAreaPreset::Action, YELLOW).unwrap();
    assert_eq!(action, vec![guide(4, 4, 93, 93, YELLOW)]);
    let title = preset_guides(101, 101, SafeAreaPreset::Title, YELLOW).unwrap();
    assert_eq!(title, vec![guide(5, 5, 90, 90, YELLOW)]);

    // 5 * 93 / 100 = 4 and 3 * 93 / 100 = 2; truncating centering keeps (0, 0).
    let smpte = preset_guides(5, 3, SafeAreaPreset::Smpte, YELLOW).unwrap();
    assert_eq!(
        smpte,
        vec![guide(0, 0, 4, 2, YELLOW), guide(0, 0, 4, 2, YELLOW),]
    );
    assert!(
        smpte
            .iter()
            .all(|guide| guide.width > 0 && guide.height > 0),
        "odd canvases must not produce zero-size guides"
    );
}

#[test]
fn fixed_presets_reject_canvases_that_truncate_to_zero() {
    for (width, height) in [(1_u32, 1_u32), (1, 1080), (1920, 1), (1, 50), (50, 1)] {
        for preset in [
            SafeAreaPreset::Action,
            SafeAreaPreset::Title,
            SafeAreaPreset::Smpte,
        ] {
            assert!(
                preset_guides(width, height, preset, YELLOW).is_err(),
                "{preset:?} on {width}x{height}"
            );
        }
    }
    assert_eq!(
        preset_guides(1, 1, SafeAreaPreset::Action, YELLOW),
        Err(SafeAreaError::CanvasTooSmall {
            canvas_width: 1,
            canvas_height: 1,
            preset: SafeAreaPreset::Action,
        })
    );
    assert_eq!(
        preset_guides(1, 1080, SafeAreaPreset::Title, YELLOW),
        Err(SafeAreaError::CanvasTooSmall {
            canvas_width: 1,
            canvas_height: 1080,
            preset: SafeAreaPreset::Title,
        })
    );
    // 2x2 is the smallest canvas whose action/title sizes truncate to one pixel.
    assert_eq!(
        preset_guides(2, 2, SafeAreaPreset::Action, YELLOW).unwrap(),
        vec![guide(0, 0, 1, 1, YELLOW)]
    );
}

#[test]
fn custom_presets_use_exact_basis_point_geometry() {
    let full = preset_guides(1920, 1080, custom(0, 0, 10_000, 10_000), YELLOW).unwrap();
    assert_eq!(full, vec![guide(0, 0, 1920, 1080, YELLOW)]);

    // 1920 * 5_000 / 10_000 = 960 and 1080 * 5_000 / 10_000 = 540, so the
    // rectangle ends exactly on the bottom-right canvas edge.
    let corner = preset_guides(1920, 1080, custom(5_000, 5_000, 5_000, 5_000), MAGENTA).unwrap();
    assert_eq!(corner, vec![guide(960, 540, 960, 540, MAGENTA)]);

    // 101 * 2_500 / 10_000 = 25.25 -> 25 and 101 * 5_000 / 10_000 = 50.5 -> 50.
    let odd = preset_guides(101, 101, custom(2_500, 9_000, 5_000, 1_000), YELLOW).unwrap();
    assert_eq!(odd, vec![guide(25, 90, 50, 10, YELLOW)]);

    // Full-canvas arithmetic stays overflow-free at the largest u32 canvas.
    let huge = preset_guides(u32::MAX, u32::MAX, custom(0, 0, 10_000, 10_000), YELLOW).unwrap();
    assert_eq!(huge, vec![guide(0, 0, u32::MAX, u32::MAX, YELLOW)]);
}

#[test]
fn custom_presets_reject_empty_out_of_bounds_and_out_of_range_requests() {
    assert_eq!(
        preset_guides(1920, 1080, custom(0, 0, 0, 10_000), YELLOW),
        Err(SafeAreaError::CustomGuideEmpty {
            width: 0,
            height: 1080,
        })
    );
    assert_eq!(
        preset_guides(1920, 1080, custom(0, 0, 10_000, 0), YELLOW),
        Err(SafeAreaError::CustomGuideEmpty {
            width: 1920,
            height: 0,
        })
    );
    // 5 * 1 / 10_000 truncates to zero on both axes.
    assert_eq!(
        preset_guides(5, 5, custom(0, 0, 1, 1), YELLOW),
        Err(SafeAreaError::CustomGuideEmpty {
            width: 0,
            height: 0,
        })
    );

    // right = 1728 + 384 = 2112 > 1920.
    assert_eq!(
        preset_guides(1920, 1080, custom(9_000, 0, 2_000, 10_000), YELLOW),
        Err(out_of_bounds(9_000, 0, 2_000, 10_000, 1920, 1080))
    );
    // bottom = 972 + 216 = 1188 > 1080.
    assert_eq!(
        preset_guides(1920, 1080, custom(0, 9_000, 10_000, 2_000), YELLOW),
        Err(out_of_bounds(0, 9_000, 10_000, 2_000, 1920, 1080))
    );
    // x = 1920 plus width 1920 overflows the canvas even though x is on an edge.
    assert_eq!(
        preset_guides(1920, 1080, custom(10_000, 0, 10_000, 10_000), YELLOW),
        Err(out_of_bounds(10_000, 0, 10_000, 10_000, 1920, 1080))
    );
    // An offset exactly on the bottom canvas edge truncates the one-basis-point
    // height to zero pixels, so it is empty even though the offset is on the edge.
    assert_eq!(
        preset_guides(1920, 1080, custom(0, 10_000, 10_000, 1), YELLOW),
        Err(SafeAreaError::CustomGuideEmpty {
            width: 1920,
            height: 0,
        })
    );
    // A bottom-edge offset with a height that survives truncation leaves the canvas.
    assert_eq!(
        preset_guides(1920, 1080, custom(0, 10_000, 10_000, 1_000), YELLOW),
        Err(out_of_bounds(0, 10_000, 10_000, 1_000, 1920, 1080))
    );

    // Basis points above 10_000 cannot describe a rectangle inside the canvas.
    assert_eq!(
        preset_guides(1920, 1080, custom(10_001, 0, 1, 1), YELLOW),
        Err(out_of_bounds(10_001, 0, 1, 1, 1920, 1080))
    );
    assert_eq!(
        preset_guides(1920, 1080, custom(0, 0, u16::MAX, u16::MAX), YELLOW),
        Err(out_of_bounds(0, 0, u16::MAX, u16::MAX, 1920, 1080))
    );
}

#[test]
fn zero_canvas_dimensions_are_rejected_for_every_preset() {
    let presets = [
        SafeAreaPreset::Action,
        SafeAreaPreset::Title,
        SafeAreaPreset::Smpte,
        custom(0, 0, 10_000, 10_000),
    ];
    for preset in presets {
        assert_eq!(
            preset_guides(0, 1080, preset, YELLOW),
            Err(SafeAreaError::ZeroCanvasWidth),
            "{preset:?}"
        );
        assert_eq!(
            preset_guides(1920, 0, preset, YELLOW),
            Err(SafeAreaError::ZeroCanvasHeight),
            "{preset:?}"
        );
        assert_eq!(
            preset_guides(0, 0, preset, YELLOW),
            Err(SafeAreaError::ZeroCanvasWidth),
            "{preset:?}"
        );
    }
}

#[test]
fn push_preset_guides_appends_generated_guides_and_returns_the_count() {
    let mut scene = Scene::new(1920, 1080, BACKGROUND).unwrap();
    let pushed = push_preset_guides(&mut scene, SafeAreaPreset::Smpte, YELLOW).unwrap();
    assert_eq!(pushed, 2);
    assert_eq!(
        scene.safe_areas(),
        &[
            guide(67, 38, 1785, 1004, YELLOW),
            guide(96, 54, 1728, 972, YELLOW),
        ][..]
    );

    let pushed = push_preset_guides(&mut scene, SafeAreaPreset::Title, MAGENTA).unwrap();
    assert_eq!(pushed, 1);
    assert_eq!(scene.safe_areas()[2], guide(96, 54, 1728, 972, MAGENTA));
}

#[test]
fn push_preset_guides_preflights_capacity_without_partial_application() {
    assert_eq!(MAX_SAFE_AREA_GUIDES, CompositionPlan::MAX_SAFE_AREAS);

    let mut scene = Scene::new(1920, 1080, BACKGROUND).unwrap();
    for _ in 0..MAX_SAFE_AREA_GUIDES - 1 {
        scene.push_safe_area(guide(0, 0, 1, 1, YELLOW));
    }
    let before = scene.safe_areas().to_vec();
    assert_eq!(
        push_preset_guides(&mut scene, SafeAreaPreset::Smpte, YELLOW),
        Err(SafeAreaError::TooManyGuides {
            existing: MAX_SAFE_AREA_GUIDES - 1,
            requested: 2,
            maximum: MAX_SAFE_AREA_GUIDES,
        })
    );
    assert_eq!(
        scene.safe_areas(),
        &before[..],
        "a rejected push must not apply part of the generated guides"
    );

    // Filling to exactly the capacity succeeds, and one more guide is rejected.
    let mut scene = Scene::new(1920, 1080, BACKGROUND).unwrap();
    for _ in 0..MAX_SAFE_AREA_GUIDES - 2 {
        scene.push_safe_area(guide(0, 0, 1, 1, YELLOW));
    }
    assert_eq!(
        push_preset_guides(&mut scene, SafeAreaPreset::Smpte, YELLOW),
        Ok(2)
    );
    assert_eq!(scene.safe_areas().len(), MAX_SAFE_AREA_GUIDES);
    let full = scene.safe_areas().to_vec();
    assert_eq!(
        push_preset_guides(&mut scene, SafeAreaPreset::Title, YELLOW),
        Err(SafeAreaError::TooManyGuides {
            existing: MAX_SAFE_AREA_GUIDES,
            requested: 1,
            maximum: MAX_SAFE_AREA_GUIDES,
        })
    );
    assert_eq!(scene.safe_areas(), &full[..]);

    // Geometry errors are preflighted too: nothing is pushed on a tiny canvas.
    let mut tiny = Scene::new(1, 1, BACKGROUND).unwrap();
    assert!(push_preset_guides(&mut tiny, SafeAreaPreset::Smpte, YELLOW).is_err());
    assert_eq!(tiny.safe_areas(), []);
}

#[test]
fn generated_safe_areas_route_to_operator_only() {
    let mut scene = Scene::new(1920, 1080, BACKGROUND).unwrap();
    push_preset_guides(&mut scene, SafeAreaPreset::Smpte, YELLOW).unwrap();
    let expected = [
        guide(67, 38, 1785, 1004, YELLOW),
        guide(96, 54, 1728, 972, YELLOW),
    ];

    let (operator, operator_report) = compile_scene(&scene, OutputTarget::Operator).unwrap();
    assert_eq!(operator.safe_areas(), &expected[..]);
    assert!(
        operator_report
            .entries()
            .contains(&ReportEntry::SafeAreasIncluded { count: 2 })
    );
    let operator_frame = execute_cpu(&operator, &[]).unwrap();
    assert_eq!(operator_frame.pixel(67, 38), Some(YELLOW));
    assert_eq!(operator_frame.pixel(96, 54), Some(YELLOW));
    assert_eq!(operator_frame.pixel(0, 0), Some(BACKGROUND));

    for target in [
        OutputTarget::Program,
        OutputTarget::Preview,
        OutputTarget::Record,
        OutputTarget::Stream,
    ] {
        let (plan, report) = compile_scene(&scene, target).unwrap();
        assert!(plan.safe_areas().is_empty(), "{target:?}");
        assert!(
            report
                .entries()
                .contains(&ReportEntry::SafeAreasOmitted { count: 2, target }),
            "{target:?}"
        );
    }
    let (program, _) = compile_scene(&scene, OutputTarget::Program).unwrap();
    let program_frame = execute_cpu(&program, &[]).unwrap();
    assert_eq!(program_frame.pixel(67, 38), Some(BACKGROUND));
    assert_eq!(program_frame.pixel(96, 54), Some(BACKGROUND));
}

#[test]
fn preset_generation_is_deterministic_across_calls() {
    let presets = [
        SafeAreaPreset::Action,
        SafeAreaPreset::Title,
        SafeAreaPreset::Smpte,
        custom(1_234, 2_345, 6_789, 1_234),
    ];
    for preset in presets {
        let first = preset_guides(1919, 1079, preset, MAGENTA).unwrap();
        let second = preset_guides(1919, 1079, preset, MAGENTA).unwrap();
        assert_eq!(first, second, "{preset:?}");
    }

    let mut scene = Scene::new(1920, 1080, BACKGROUND).unwrap();
    assert_eq!(
        push_preset_guides(&mut scene, SafeAreaPreset::Smpte, YELLOW),
        Ok(2)
    );
    let first = scene.safe_areas().to_vec();
    let mut scene = Scene::new(1920, 1080, BACKGROUND).unwrap();
    assert_eq!(
        push_preset_guides(&mut scene, SafeAreaPreset::Smpte, YELLOW),
        Ok(2)
    );
    assert_eq!(scene.safe_areas(), &first[..]);
}

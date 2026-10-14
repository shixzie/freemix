//! SW-019 colour grading on the CPU reference compositor: grades run after
//! the effect chain, on straight colour, before premultiplication, and never
//! change alpha.

use std::sync::Arc;

use fm_compositor::{
    AlphaMode, CompiledGrade, CompositionPlan, CpuSourceFrame, Effect, OutputTarget,
    ReferenceEffect, Rgba8, Rotation, Scene, SourceId, SourceLayer, Transform, apply_grade,
    apply_reference_effect, compile_scene, execute_cpu,
};
use fm_grade::{Contrast, Grade, Wheel, WhiteBalance};
use fm_video::ImageFrame;

const SOURCE: SourceId = SourceId::new(1);
const WIDTH: u32 = 8;
const HEIGHT: u32 = 4;

#[allow(clippy::cast_possible_truncation)]
fn straight_pattern() -> ImageFrame {
    const ALPHAS: [u8; 5] = [0, 1, 64, 200, 255];
    let mut bytes = Vec::new();
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            bytes.extend_from_slice(&[
                ((x * 37 + y * 11) % 256) as u8,
                ((x * 13 + y * 71 + 40) % 256) as u8,
                ((x * 91 + y * 29 + 7) % 256) as u8,
                ALPHAS[((x + y * 3) % 5) as usize],
            ]);
        }
    }
    ImageFrame::new(WIDTH, HEIGHT, WIDTH as usize * 4, bytes).unwrap()
}

fn map_pixels(frame: &ImageFrame, map: impl Fn([u8; 4]) -> [u8; 4]) -> ImageFrame {
    let mut bytes = Vec::new();
    for y in 0..frame.height() {
        for x in 0..frame.width() {
            bytes.extend_from_slice(&map(frame.pixel(x, y).unwrap().to_bytes()));
        }
    }
    ImageFrame::new(
        frame.width(),
        frame.height(),
        frame.width() as usize * 4,
        bytes,
    )
    .unwrap()
}

fn multiply(channel: u8, alpha: u8) -> u8 {
    u8::try_from((u16::from(channel) * u16::from(alpha) + 127) / 255).unwrap()
}

fn unmultiply(channel: u8, alpha: u8) -> u8 {
    if alpha == 0 {
        return 0;
    }
    u8::try_from(((u16::from(channel) * 255 + u16::from(alpha) / 2) / u16::from(alpha)).min(255))
        .unwrap()
}

fn premultiply(frame: &ImageFrame) -> ImageFrame {
    map_pixels(frame, |[r, g, b, a]| {
        [multiply(r, a), multiply(g, a), multiply(b, a), a]
    })
}

fn unpremultiply(frame: &ImageFrame) -> ImageFrame {
    map_pixels(frame, |[r, g, b, a]| {
        [unmultiply(r, a), unmultiply(g, a), unmultiply(b, a), a]
    })
}

fn compiled(grade: &Grade) -> Arc<CompiledGrade> {
    Arc::new(CompiledGrade::new(grade, None).unwrap())
}

fn warm_contrast() -> Arc<CompiledGrade> {
    compiled(&Grade {
        white_balance: WhiteBalance::new(400, 0),
        exposure_millistops: -700,
        contrast: Contrast::new(300, 5000),
        saturation_permille: 1400,
        wheels: fm_grade::Wheels {
            lift: Wheel::new(0, 0, 80),
            ..fm_grade::Wheels::NEUTRAL
        },
        ..Grade::IDENTITY
    })
}

/// One full-canvas layer over a transparent background, so the composed
/// output equals the layer's premultiplied pixels exactly.
fn plan(layer: SourceLayer) -> CompositionPlan {
    let mut scene = Scene::new(WIDTH, HEIGHT, Rgba8::new(0, 0, 0, 0)).unwrap();
    scene.push_layer(layer);
    compile_scene(&scene, OutputTarget::Program).unwrap().0
}

fn layer() -> SourceLayer {
    SourceLayer::new(
        SOURCE,
        0,
        Transform::new(0, 0, WIDTH, HEIGHT, Rotation::Deg0),
    )
}

fn render(plan: &CompositionPlan, source: &ImageFrame) -> ImageFrame {
    execute_cpu(plan, &[CpuSourceFrame::new(SOURCE, source)]).unwrap()
}

#[test]
fn apply_grade_matches_the_compiled_grade_and_keeps_alpha() {
    let source = straight_pattern();
    let grade = warm_contrast();
    let graded = apply_grade(&source, &grade).unwrap();
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let before = source.pixel(x, y).unwrap();
            let after = graded.pixel(x, y).unwrap();
            assert_eq!(after.a, before.a);
            assert_eq!(
                [after.r, after.g, after.b],
                grade.apply_rgb8([before.r, before.g, before.b])
            );
            assert_eq!(
                [after.r, after.g, after.b],
                fm_grade::reference::evaluate_pixel(
                    grade.grade(),
                    None,
                    [before.r, before.g, before.b]
                )
            );
        }
    }
}

#[test]
fn apply_grade_handles_padded_strides() {
    let packed = straight_pattern();
    let stride = WIDTH as usize * 4 + 12;
    let mut bytes = vec![0xAB; stride * HEIGHT as usize];
    for (row, chunk) in packed.pixels().chunks(WIDTH as usize * 4).enumerate() {
        bytes[row * stride..row * stride + chunk.len()].copy_from_slice(chunk);
    }
    let padded = ImageFrame::new(WIDTH, HEIGHT, stride, bytes).unwrap();
    let grade = warm_contrast();
    assert_eq!(
        apply_grade(&padded, &grade).unwrap(),
        apply_grade(&packed, &grade).unwrap()
    );
    // The identity grade is an exact copy, padding included.
    let identity = CompiledGrade::identity();
    assert_eq!(apply_grade(&padded, &identity).unwrap(), padded);
}

#[test]
fn straight_layers_grade_before_premultiplying() {
    let source = straight_pattern();
    let grade = warm_contrast();
    let output = render(&plan(layer().with_grade(grade.clone())), &source);
    let expected = premultiply(&apply_grade(&source, &grade).unwrap());
    assert_eq!(output, expected);
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            assert_eq!(output.pixel(x, y).unwrap().a, source.pixel(x, y).unwrap().a);
        }
    }
}

#[test]
fn premultiplied_layers_grade_straight_colour() {
    let source = premultiply(&straight_pattern());
    let grade = warm_contrast();
    let output = render(
        &plan(
            layer()
                .with_alpha_mode(AlphaMode::Premultiplied)
                .with_grade(grade.clone()),
        ),
        &source,
    );
    let expected = premultiply(&apply_grade(&unpremultiply(&source), &grade).unwrap());
    assert_eq!(output, expected);
    // Grading premultiplied bytes directly would differ for translucent
    // pixels, so this pins the straighten step.
    let naive = apply_grade(&source, &grade).unwrap();
    assert_ne!(output, naive);
}

#[test]
fn grades_run_after_the_effect_chain() {
    let source = map_pixels(&straight_pattern(), |[r, g, b, _]| [r, g, b, 255]);
    let grade = compiled(&Grade {
        exposure_millistops: -1500,
        saturation_permille: 0,
        ..Grade::IDENTITY
    });
    let effect = ReferenceEffect::BrightnessContrast {
        brightness: 60,
        contrast: 40,
    };
    let output = render(
        &plan(
            layer()
                .with_effect(Effect::new("brightness_contrast", vec![60, 40]))
                .with_grade(grade.clone()),
        ),
        &source,
    );
    let effect_then_grade =
        apply_grade(&apply_reference_effect(&source, effect).unwrap(), &grade).unwrap();
    let grade_then_effect =
        apply_reference_effect(&apply_grade(&source, &grade).unwrap(), effect).unwrap();
    assert_eq!(output, effect_then_grade);
    assert_ne!(output, grade_then_effect);
}

#[test]
fn identity_and_disabled_grades_leave_composition_bit_exact() {
    let straight = straight_pattern();
    let premultiplied = premultiply(&straight);
    let disabled = compiled(&Grade {
        enabled: false,
        saturation_permille: 0,
        ..Grade::IDENTITY
    });
    for grade in [Arc::new(CompiledGrade::identity()), disabled] {
        assert_eq!(
            render(&plan(layer().with_grade(grade.clone())), &straight),
            render(&plan(layer()), &straight)
        );
        let premultiplied_layer = layer().with_alpha_mode(AlphaMode::Premultiplied);
        assert_eq!(
            render(
                &plan(premultiplied_layer.clone().with_grade(grade)),
                &premultiplied
            ),
            render(&plan(premultiplied_layer), &premultiplied)
        );
    }
}

#[test]
fn compiled_plans_carry_the_layer_grade() {
    let grade = warm_contrast();
    let plan = plan(layer().with_grade(grade.clone()));
    let planned = plan.layers()[0].grade().unwrap();
    assert!(Arc::ptr_eq(planned, &grade));
    assert!(plan.layers()[0].grade().is_some());
    assert!(layer().grade().is_none());
}

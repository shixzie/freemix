//! GX-011 arbitrary-angle layer rotation on the CPU reference path.
//!
//! A tilted layer turns its transform footprint clockwise about the
//! footprint centre and samples it bilinearly; whole turns and exact
//! quarter turns are pixel-exact.

use fm_compositor::{
    CpuSourceFrame, OutputTarget, Rgba8, Rotation, Scene, SourceId, SourceLayer, Transform,
    compile_scene, execute_cpu,
};
use fm_video::ImageFrame;

const SOURCE: SourceId = SourceId::new(1);
const BACKGROUND: Rgba8 = Rgba8::new(0, 0, 0, 255);

/// `r = x * 20`, `g = y * 30`: every texel distinct.
fn coordinates(width: u32, height: u32) -> ImageFrame {
    let bytes = (0..height)
        .flat_map(|y| {
            (0..width).flat_map(move |x| {
                Rgba8::new(
                    u8::try_from(x * 20).unwrap(),
                    u8::try_from(y * 30).unwrap(),
                    90,
                    255,
                )
                .to_bytes()
            })
        })
        .collect();
    ImageFrame::new(width, height, usize::try_from(width).unwrap() * 4, bytes).unwrap()
}

fn render(layer: SourceLayer, source: &ImageFrame) -> ImageFrame {
    let mut scene = Scene::new(24, 20, BACKGROUND).unwrap();
    scene.push_layer(layer);
    let (plan, _) = compile_scene(&scene, OutputTarget::Program).unwrap();
    execute_cpu(&plan, &[CpuSourceFrame::new(SOURCE, source)]).unwrap()
}

fn layer(x: i32, y: i32, width: u32, height: u32, rotation: Rotation) -> SourceLayer {
    SourceLayer::new(SOURCE, 0, Transform::new(x, y, width, height, rotation))
}

#[test]
fn whole_turns_draw_exactly_as_untilted() {
    let source = coordinates(6, 4);
    let untilted = render(layer(3, 5, 12, 8, Rotation::Deg90), &source);
    for millidegrees in [360_000, -360_000, 720_000] {
        assert_eq!(
            render(
                layer(3, 5, 12, 8, Rotation::Deg90).with_rotation_millidegrees(millidegrees),
                &source
            ),
            untilted,
            "{millidegrees}"
        );
    }
}

#[test]
fn a_half_turn_tilt_is_the_half_turn_transform() {
    // Any size: a half turn about the centre is a pixel permutation.
    let source = coordinates(5, 3);
    assert_eq!(
        render(
            layer(4, 6, 11, 7, Rotation::Deg0).with_rotation_millidegrees(180_000),
            &source
        ),
        render(layer(4, 6, 11, 7, Rotation::Deg180), &source)
    );
}

#[test]
fn a_quarter_turn_tilt_turns_the_footprint_about_its_centre() {
    // 12x8 differs by an even number of pixels, so a quarter turn about the
    // centre lands the 8x12 footprint on whole pixels, two to the right and
    // two up.
    let source = coordinates(6, 4);
    for (tilt, turned) in [(90_000, Rotation::Deg90), (-90_000, Rotation::Deg270)] {
        assert_eq!(
            render(
                layer(4, 6, 12, 8, Rotation::Deg0).with_rotation_millidegrees(tilt),
                &source
            ),
            render(layer(6, 4, 12, 8, turned), &source),
            "{tilt}"
        );
    }
}

#[test]
#[allow(clippy::cast_precision_loss)]
fn an_eighth_turn_keeps_the_centre_and_covers_the_area() {
    let white = ImageFrame::new(1, 1, 4, vec![255; 4]).unwrap();
    let transparent = Scene::new(24, 20, Rgba8::new(0, 0, 0, 0)).unwrap();
    let mut scene = transparent.clone();
    scene.push_layer(layer(6, 6, 12, 8, Rotation::Deg0).with_rotation_millidegrees(45_000));
    let (plan, _) = compile_scene(&scene, OutputTarget::Program).unwrap();
    let output = execute_cpu(&plan, &[CpuSourceFrame::new(SOURCE, &white)]).unwrap();
    // The centre is fully covered; a pixel beside the untilted top-left corner is not.
    assert_eq!(
        output.pixel(12, 10).unwrap(),
        Rgba8::new(255, 255, 255, 255)
    );
    assert_eq!(output.pixel(5, 5).unwrap().a, 0);
    // Bilinear coverage keeps the 96-pixel area to within two pixels.
    let coverage: u32 = output
        .pixels()
        .as_chunks::<4>()
        .0
        .iter()
        .map(|pixel| u32::from(pixel[3]))
        .sum();
    let area = f64::from(coverage) / 255.0;
    assert!((area - 96.0).abs() < 2.0, "covered area {area}");
    // Clockwise: the layer's right end swings down, so the pixel right of
    // centre and below the untilted box is covered.
    assert_eq!(output.pixel(15, 13).unwrap().a, 255);
    assert_eq!(output.pixel(15, 6).unwrap().a, 0);
}

#[test]
fn a_tilted_layer_keeps_opacity_and_its_border() {
    let source = coordinates(4, 4);
    let tilted = render(
        layer(4, 4, 14, 10, Rotation::Deg0)
            .with_rotation_millidegrees(10_000)
            .with_inset_border(1)
            .with_opacity(128),
        &source,
    );
    // The border is white at half opacity over black near the centre-top
    // edge, which a 10 degree turn barely moves.
    let top = tilted.pixel(11, 4).unwrap();
    assert!(top.r > 100 && top.r < 140, "{top:?}");
}

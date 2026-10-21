//! Exact-pixel CPU-oracle coverage for the chroma and luma key stage.
//!
//! Expectations are hand-computed from the integer formulas in
//! `crates/gpu/fm-compositor/src/cpu.rs`: the chroma distance is the larger
//! absolute difference of the fixed-point BT.709 Cb and Cr from the key
//! color's ([`fm_compositor::chroma_distance`]; against pure green
//! `(0, 255, 0)`, Cb 29 and Cr 12, the pixel `(0, 255, 2 d)` is at
//! distance `d` for `d <= 10`), the ramp adds
//! `softness / 2` before truncating, spill uses
//! `(spill * (255 - key_alpha) + 127) / 255`, and every channel scaling uses
//! `(left * right + 127) / 255`.

use fm_compositor::{
    AlphaMode, ChromaKey, CompositionPlan, CpuSourceFrame, Effect, Key, LumaKey, OutputTarget,
    RectMask, Rgba8, Rotation, Scene, SourceId, SourceLayer, Transform, compile_scene, execute_cpu,
};
use fm_video::ImageFrame;

fn frame(width: u32, height: u32, pixels: &[Rgba8]) -> ImageFrame {
    ImageFrame::new(
        width,
        height,
        usize::try_from(width).unwrap() * 4,
        pixels.iter().flat_map(|pixel| pixel.to_bytes()).collect(),
    )
    .unwrap()
}

fn padded_frame(width: u32, height: u32, pixels: &[Rgba8], padding: usize) -> ImageFrame {
    let stride = usize::try_from(width).unwrap() * 4 + padding;
    let mut bytes = Vec::with_capacity(stride * usize::try_from(height).unwrap());
    for row in pixels.chunks_exact(usize::try_from(width).unwrap()) {
        for pixel in row {
            bytes.extend_from_slice(&pixel.to_bytes());
        }
        bytes.extend(std::iter::repeat_n(0xAB_u8, padding));
    }
    ImageFrame::new(width, height, stride, bytes).unwrap()
}

fn pixels(frame: &ImageFrame) -> Vec<Rgba8> {
    (0..frame.height())
        .flat_map(|y| (0..frame.width()).map(move |x| frame.pixel(x, y).unwrap()))
        .collect()
}

fn transform(x: i32, y: i32, width: u32, height: u32) -> Transform {
    Transform::new(x, y, width, height, Rotation::Deg0)
}

fn plan_with_layers(width: u32, height: u32, layers: Vec<SourceLayer>) -> CompositionPlan {
    let mut scene = Scene::new(width, height, Rgba8::new(0, 0, 0, 0)).unwrap();
    for layer in layers {
        scene.push_layer(layer);
    }
    compile_scene(&scene, OutputTarget::Program).unwrap().0
}

fn execute_sources(plan: &CompositionPlan, sources: &[(SourceId, &ImageFrame)]) -> ImageFrame {
    let bindings = sources
        .iter()
        .map(|(source, frame)| CpuSourceFrame::new(*source, frame))
        .collect::<Vec<_>>();
    execute_cpu(plan, &bindings).unwrap()
}

fn execute_layer(plan: &CompositionPlan, source: &ImageFrame) -> ImageFrame {
    execute_sources(plan, &[(SourceId::new(1), source)])
}

fn chroma(color: Rgba8, tolerance: u8, softness: u8, spill: u8) -> Key {
    Key::Chroma(ChromaKey::new(color, tolerance, softness, spill))
}

fn luma(threshold: u8, softness: u8, invert: bool) -> Key {
    Key::Luma(LumaKey::new(threshold, softness, invert))
}

fn keyed_row(width: u32, key: Key) -> CompositionPlan {
    plan_with_layers(
        width,
        1,
        vec![SourceLayer::new(SourceId::new(1), 0, transform(0, 0, width, 1)).with_key(key)],
    )
}

#[test]
fn chroma_key_exact_color_transparent_and_tolerance_boundary_is_exact() {
    let green = Rgba8::new(0, 255, 0, 255);
    let exact = keyed_row(5, chroma(green, 0, 0, 0));
    assert_eq!(
        exact.layers()[0].key(),
        Some(Key::Chroma(ChromaKey::new(green, 0, 0, 0)))
    );
    let source = frame(
        5,
        1,
        &[
            Rgba8::new(0, 255, 0, 255),   // distance 0
            Rgba8::new(0, 254, 0, 255),   // distance 1: untouched by tolerance 0
            Rgba8::new(1, 255, 0, 255),   // Cr 13: distance 1
            Rgba8::new(0, 255, 4, 255),   // Cb 31: distance 2
            Rgba8::new(255, 0, 255, 255), // far from the key color
        ],
    );
    assert_eq!(
        pixels(&execute_layer(&exact, &source)),
        [
            Rgba8::new(0, 0, 0, 0),
            Rgba8::new(0, 254, 0, 255),
            Rgba8::new(1, 255, 0, 255),
            Rgba8::new(0, 255, 4, 255),
            Rgba8::new(255, 0, 255, 255),
        ]
    );

    let tolerant = keyed_row(4, chroma(green, 2, 0, 0));
    let source = frame(
        4,
        1,
        &[
            Rgba8::new(0, 255, 0, 255), // distance 0 <= tolerance
            Rgba8::new(0, 255, 4, 255), // distance 2 == tolerance
            Rgba8::new(2, 255, 0, 255), // Cr 13: distance 1 < tolerance
            Rgba8::new(0, 255, 6, 255), // distance 3: just past tolerance, hard edge
        ],
    );
    assert_eq!(
        pixels(&execute_layer(&tolerant, &source)),
        [
            Rgba8::new(0, 0, 0, 0),
            Rgba8::new(0, 0, 0, 0),
            Rgba8::new(0, 0, 0, 0),
            Rgba8::new(0, 255, 6, 255),
        ]
    );

    // Shading moves a pixel less than its RGB difference: half-bright green
    // (Cb/Cr (79, 70)) is distance 58 from pure green, not 127, so a
    // similarity of 60 keys it.
    let shade = keyed_row(1, chroma(green, 60, 0, 0));
    let dark = frame(1, 1, &[Rgba8::new(0, 128, 0, 255)]);
    assert_eq!(
        pixels(&execute_layer(&shade, &dark)),
        [Rgba8::new(0, 0, 0, 0)]
    );
}

#[test]
fn chroma_softness_ramp_rounds_half_up_at_exact_midpoint() {
    // Ramp alpha is ((distance - tolerance) * 255 + softness / 2) / softness.
    // Distance 2 over softness 4 lands on exactly 127.5, which rounds to 128.
    let ramped = keyed_row(6, chroma(Rgba8::new(0, 255, 0, 255), 0, 4, 0));
    let source = frame(
        6,
        1,
        &[
            Rgba8::new(0, 255, 0, 255), // distance 0 -> 0
            Rgba8::new(0, 255, 2, 255), // distance 1 -> (255 + 2) / 4 = 64
            Rgba8::new(0, 255, 4, 255), // distance 2 -> (510 + 2) / 4 = 128
            Rgba8::new(0, 255, 6, 255), // distance 3 -> (765 + 2) / 4 = 191
            Rgba8::new(0, 255, 8, 255), // distance 4 == softness -> 255
            Rgba8::new(0, 0, 0, 255),   // distance 116 >= softness -> 255
        ],
    );
    assert_eq!(
        pixels(&execute_layer(&ramped, &source)),
        [
            Rgba8::new(0, 0, 0, 0),
            Rgba8::new(0, 64, 1, 64),
            Rgba8::new(0, 128, 2, 128),
            Rgba8::new(0, 191, 4, 191),
            Rgba8::new(0, 255, 8, 255),
            Rgba8::new(0, 0, 0, 255),
        ]
    );

    // softness 0 is a hard edge: the tolerance boundary stays keyed and the
    // next distance is fully opaque.
    let hard = keyed_row(2, chroma(Rgba8::new(0, 255, 0, 255), 10, 0, 0));
    let source = frame(
        2,
        1,
        &[Rgba8::new(0, 255, 20, 255), Rgba8::new(0, 255, 22, 255)],
    );
    assert_eq!(
        pixels(&execute_layer(&hard, &source)),
        [Rgba8::new(0, 0, 0, 0), Rgba8::new(0, 255, 22, 255)]
    );
}

#[test]
fn chroma_spill_suppresses_only_when_enabled() {
    // Key color (100, 255, 150) is green-dominant, so spill pulls green toward
    // max(r, b). The pixel's Cb/Cr (170, 146) sit at distance 91 from the
    // key's (93, 55) with softness 255, so the straight-alpha key reduces
    // alpha to 91 before premultiplication.
    let key_color = Rgba8::new(100, 255, 150, 255);
    let partial = frame(1, 1, &[Rgba8::new(100, 55, 150, 255)]);

    let no_spill = keyed_row(1, chroma(key_color, 0, 255, 0));
    assert_eq!(
        pixels(&execute_layer(&no_spill, &partial)),
        [Rgba8::new(36, 20, 54, 91)]
    );

    // spill 255: amount = (255 * (255 - 91) + 127) / 255 = 164, and
    // green = (55 * 91 + 150 * 164 + 127) / 255 = 116 before premultiplication.
    let with_spill = keyed_row(1, chroma(key_color, 0, 255, 255));
    assert_eq!(
        pixels(&execute_layer(&with_spill, &partial)),
        [Rgba8::new(36, 41, 54, 91)]
    );

    // A fully opaque key alpha (a hard edge) skips spill suppression.
    let hard_spill = keyed_row(1, chroma(key_color, 0, 0, 255));
    let full = frame(1, 1, &[Rgba8::new(100, 0, 150, 255)]);
    assert_eq!(
        pixels(&execute_layer(&hard_spill, &full)),
        [Rgba8::new(100, 0, 150, 255)]
    );

    // Red-dominant key color pulls red toward max(g, b).
    let red_dominant = keyed_row(1, chroma(Rgba8::new(255, 50, 50, 255), 0, 255, 255));
    let source = frame(1, 1, &[Rgba8::new(55, 200, 100, 255)]);
    assert_eq!(
        pixels(&execute_layer(&red_dominant, &source)),
        [Rgba8::new(69, 134, 67, 171)]
    );

    // Blue-dominant key color pulls blue toward max(r, g).
    let blue_dominant = keyed_row(1, chroma(Rgba8::new(50, 50, 255, 255), 0, 255, 255));
    let source = frame(1, 1, &[Rgba8::new(200, 100, 55, 255)]);
    assert_eq!(
        pixels(&execute_layer(&blue_dominant, &source)),
        [Rgba8::new(107, 54, 66, 137)]
    );
}

#[test]
fn luma_key_threshold_softness_and_invert_are_exact() {
    // luma = (54 * r + 183 * g + 19 * b + 128) / 256 with truncating division.
    let ramped = keyed_row(6, luma(92, 4, false));
    let source = frame(
        6,
        1,
        &[
            Rgba8::new(0, 128, 0, 255), // luma 92 == threshold -> 0
            Rgba8::new(0, 129, 0, 255), // luma 92 -> 0
            Rgba8::new(0, 130, 0, 255), // luma 93 -> (255 + 2) / 4 = 64
            Rgba8::new(0, 131, 0, 255), // luma 94 -> (510 + 2) / 4 = 128
            Rgba8::new(0, 133, 0, 255), // luma 95 -> (765 + 2) / 4 = 191
            Rgba8::new(0, 134, 0, 255), // luma 96 == threshold + softness -> 255
        ],
    );
    assert_eq!(
        pixels(&execute_layer(&ramped, &source)),
        [
            Rgba8::new(0, 0, 0, 0),
            Rgba8::new(0, 0, 0, 0),
            Rgba8::new(0, 33, 0, 64),
            Rgba8::new(0, 66, 0, 128),
            Rgba8::new(0, 100, 0, 191),
            Rgba8::new(0, 134, 0, 255),
        ]
    );

    // softness 0 keys exactly the threshold and makes the next luma opaque.
    let hard = keyed_row(2, luma(93, 0, false));
    let source = frame(
        2,
        1,
        &[Rgba8::new(0, 130, 0, 255), Rgba8::new(0, 131, 0, 255)],
    );
    assert_eq!(
        pixels(&execute_layer(&hard, &source)),
        [Rgba8::new(0, 0, 0, 0), Rgba8::new(0, 131, 0, 255)]
    );

    // invert replaces the ramp alpha with 255 - alpha.
    let inverted = keyed_row(4, luma(92, 4, true));
    let source = frame(
        4,
        1,
        &[
            Rgba8::new(0, 128, 0, 255), // 255 - 0 = 255
            Rgba8::new(0, 130, 0, 255), // 255 - 64 = 191
            Rgba8::new(0, 131, 0, 255), // 255 - 128 = 127
            Rgba8::new(0, 134, 0, 255), // 255 - 255 = 0
        ],
    );
    assert_eq!(
        pixels(&execute_layer(&inverted, &source)),
        [
            Rgba8::new(0, 128, 0, 255),
            Rgba8::new(0, 97, 0, 191),
            Rgba8::new(0, 65, 0, 127),
            Rgba8::new(0, 0, 0, 0),
        ]
    );

    // The 54/183/19 weights give (54, 183, 19) a luma of 144; the BT.601
    // weights (77/150/29) would give it a luma of 126 instead.
    let source = frame(1, 1, &[Rgba8::new(54, 183, 19, 255)]);
    assert_eq!(
        pixels(&execute_layer(&keyed_row(1, luma(144, 0, false)), &source)),
        [Rgba8::new(0, 0, 0, 0)]
    );
    assert_eq!(
        pixels(&execute_layer(&keyed_row(1, luma(143, 0, false)), &source)),
        [Rgba8::new(54, 183, 19, 255)]
    );
}

#[test]
fn key_and_mask_multiply_with_rounded_factors() {
    let green = Rgba8::new(0, 255, 0, 255);
    let source = frame(
        4,
        1,
        &[
            Rgba8::new(0, 255, 0, 255), // key alpha 0
            Rgba8::new(0, 255, 2, 255), // key alpha 64
            Rgba8::new(0, 255, 4, 255), // key alpha 128
            Rgba8::new(0, 255, 8, 255), // key alpha 255
        ],
    );
    let keyed_and_masked = plan_with_layers(
        4,
        1,
        vec![
            SourceLayer::new(SourceId::new(1), 0, transform(0, 0, 4, 1))
                .with_key(chroma(green, 0, 4, 0))
                .with_mask(RectMask::new(1, 0, 2, 1)),
        ],
    );
    // Inside the mask the factor is (key_alpha * 255 + 127) / 255 == key_alpha;
    // outside it is (key_alpha * 0 + 127) / 255 == 0.
    assert_eq!(
        pixels(&execute_layer(&keyed_and_masked, &source)),
        [
            Rgba8::new(0, 0, 0, 0),
            Rgba8::new(0, 64, 1, 64),
            Rgba8::new(0, 128, 2, 128),
            Rgba8::new(0, 0, 0, 0),
        ]
    );

    // A mask-only layer keeps full alpha inside the rectangle and clears it
    // outside.
    let masked = frame(
        4,
        1,
        &[
            Rgba8::new(10, 20, 30, 255),
            Rgba8::new(40, 50, 60, 255),
            Rgba8::new(70, 80, 90, 255),
            Rgba8::new(100, 110, 120, 255),
        ],
    );
    let mask_only = plan_with_layers(
        4,
        1,
        vec![
            SourceLayer::new(SourceId::new(1), 0, transform(0, 0, 4, 1))
                .with_mask(RectMask::new(2, 0, 2, 1)),
        ],
    );
    assert_eq!(
        pixels(&execute_layer(&mask_only, &masked)),
        [
            Rgba8::new(0, 0, 0, 0),
            Rgba8::new(0, 0, 0, 0),
            Rgba8::new(70, 80, 90, 255),
            Rgba8::new(100, 110, 120, 255),
        ]
    );

    // Compositing the mask-only layer above the keyed+masked layer keeps the
    // keyed partial alpha where the upper layer is transparent.
    let combined = plan_with_layers(
        4,
        1,
        vec![
            SourceLayer::new(SourceId::new(1), 0, transform(0, 0, 4, 1))
                .with_key(chroma(green, 0, 4, 0))
                .with_mask(RectMask::new(1, 0, 2, 1)),
            SourceLayer::new(SourceId::new(2), 1, transform(0, 0, 4, 1))
                .with_mask(RectMask::new(2, 0, 2, 1)),
        ],
    );
    let output = execute_sources(
        &combined,
        &[(SourceId::new(1), &source), (SourceId::new(2), &masked)],
    );
    assert_eq!(
        pixels(&output),
        [
            Rgba8::new(0, 0, 0, 0),
            Rgba8::new(0, 64, 1, 64),
            Rgba8::new(70, 80, 90, 255),
            Rgba8::new(100, 110, 120, 255),
        ]
    );
}

#[test]
fn key_runs_before_effects_and_premultiplication() {
    // The key sees the raw source: (0, 0, 4) is Cb/Cr distance 2 from the
    // black key color and ramps to alpha 128, then levels maps 4 -> 10, then
    // straight alpha premultiplies 10 * 128 / 255 = 5.
    let plan = plan_with_layers(
        2,
        1,
        vec![
            SourceLayer::new(SourceId::new(1), 0, transform(0, 0, 2, 1))
                .with_key(chroma(Rgba8::new(0, 0, 0, 255), 0, 4, 0))
                .with_effect(Effect::new("levels", vec![0, 100])),
        ],
    );
    let source = frame(2, 1, &[Rgba8::new(0, 0, 4, 255), Rgba8::new(0, 0, 6, 255)]);
    // Applying levels first would push both to distance >= 4 and produce
    // alpha 255; premultiplying before the key would leave blue 4 or 6.
    assert_eq!(
        pixels(&execute_layer(&plan, &source)),
        [Rgba8::new(0, 0, 5, 128), Rgba8::new(0, 0, 11, 191)]
    );
}

#[test]
fn straight_and_premultiplied_sources_apply_key_alpha_differently() {
    let key = chroma(Rgba8::new(0, 0, 0, 255), 0, 255, 0);
    // Cb/Cr (69, 85) against black's (128, 128): distance 59 over softness
    // 255 gives key alpha 59.
    let source = frame(1, 1, &[Rgba8::new(100, 200, 60, 220)]);

    let straight = plan_with_layers(
        1,
        1,
        vec![SourceLayer::new(SourceId::new(1), 0, transform(0, 0, 1, 1)).with_key(key)],
    );
    // Straight alpha scales alpha to (220 * 59 + 127) / 255 = 51, then
    // premultiplication scales RGB by 51.
    assert_eq!(
        pixels(&execute_layer(&straight, &source)),
        [Rgba8::new(20, 40, 12, 51)]
    );

    let premultiplied = plan_with_layers(
        1,
        1,
        vec![
            SourceLayer::new(SourceId::new(1), 0, transform(0, 0, 1, 1))
                .with_key(key)
                .with_alpha_mode(AlphaMode::Premultiplied),
        ],
    );
    // The premultiplied key decision uses straight colour: (100, 200, 60) at
    // alpha 220 un-premultiplies to (116, 232, 70), Cb/Cr (60, 78), so the
    // key alpha is 68. Premultiplied alpha then scales RGB and alpha by that
    // factor at once: 100 -> 27, 200 -> 53, 60 -> 16, 220 -> 59.
    assert_eq!(
        pixels(&execute_layer(&premultiplied, &source)),
        [Rgba8::new(27, 53, 16, 59)]
    );
}

#[test]
fn premultiplied_keys_decide_on_straight_colour() {
    let premultiplied_row = |key: Key| {
        plan_with_layers(
            2,
            1,
            vec![
                SourceLayer::new(SourceId::new(1), 0, transform(0, 0, 2, 1))
                    .with_key(key)
                    .with_alpha_mode(AlphaMode::Premultiplied),
            ],
        )
    };
    // Half-transparent pure green, premultiplied, next to opaque green. Both
    // are exactly the key colour in straight terms and must key out fully.
    let green = frame(
        2,
        1,
        &[Rgba8::new(0, 128, 0, 128), Rgba8::new(0, 255, 0, 255)],
    );
    let plan = premultiplied_row(chroma(Rgba8::new(0, 255, 0, 255), 0, 0, 0));
    assert_eq!(
        pixels(&execute_layer(&plan, &green)),
        [Rgba8::new(0, 0, 0, 0), Rgba8::new(0, 0, 0, 0)]
    );

    // Half-transparent white has straight luma 255 and survives a luma key
    // at threshold 200 just like opaque white does.
    let white = frame(
        2,
        1,
        &[
            Rgba8::new(128, 128, 128, 128),
            Rgba8::new(255, 255, 255, 255),
        ],
    );
    let plan = premultiplied_row(luma(200, 0, false));
    assert_eq!(
        pixels(&execute_layer(&plan, &white)),
        [
            Rgba8::new(128, 128, 128, 128),
            Rgba8::new(255, 255, 255, 255)
        ]
    );

    // The refined keying path (identity clip, no choke) agrees.
    let refined = plan_with_layers(
        2,
        1,
        vec![
            SourceLayer::new(SourceId::new(1), 0, transform(0, 0, 2, 1))
                .with_key(chroma(Rgba8::new(0, 255, 0, 255), 0, 0, 0))
                .with_key_refinement(fm_compositor::KeyRefinement::new().with_choke(0))
                .with_alpha_mode(AlphaMode::Premultiplied),
        ],
    );
    assert_eq!(
        pixels(&execute_layer(&refined, &green)),
        [Rgba8::new(0, 0, 0, 0), Rgba8::new(0, 0, 0, 0)]
    );
}

#[test]
fn strided_source_frames_keep_exact_pixels() {
    let source_pixels = [
        Rgba8::new(0, 255, 0, 255),
        Rgba8::new(0, 255, 2, 255),
        Rgba8::new(0, 255, 4, 255),
        Rgba8::new(10, 20, 30, 255),
    ];
    let padded = padded_frame(2, 2, &source_pixels, 4);
    assert_eq!(padded.stride(), 12);

    let plan = plan_with_layers(
        2,
        2,
        vec![
            SourceLayer::new(SourceId::new(1), 0, transform(0, 0, 2, 2)).with_key(chroma(
                Rgba8::new(0, 255, 0, 255),
                0,
                4,
                0,
            )),
        ],
    );
    let expected = [
        Rgba8::new(0, 0, 0, 0),
        Rgba8::new(0, 64, 1, 64),
        Rgba8::new(0, 128, 2, 128),
        Rgba8::new(10, 20, 30, 255),
    ];
    assert_eq!(pixels(&execute_layer(&plan, &padded)), expected);

    let tight = frame(2, 2, &source_pixels);
    assert_eq!(execute_layer(&plan, &padded), execute_layer(&plan, &tight));
}

#[test]
fn repeated_key_execution_is_byte_identical() {
    let plan = plan_with_layers(
        3,
        2,
        vec![
            SourceLayer::new(SourceId::new(1), 0, transform(0, 0, 3, 2))
                .with_key(chroma(Rgba8::new(0, 255, 0, 255), 0, 4, 128))
                .with_effect(Effect::new("levels", vec![10, 200])),
            SourceLayer::new(SourceId::new(2), 1, transform(0, 0, 3, 2))
                .with_key(luma(100, 10, true)),
        ],
    );
    let first_source = frame(
        3,
        2,
        &[
            Rgba8::new(0, 255, 0, 255),
            Rgba8::new(0, 252, 0, 255),
            Rgba8::new(80, 90, 100, 255),
            Rgba8::new(10, 20, 30, 255),
            Rgba8::new(200, 100, 50, 255),
            Rgba8::new(1, 2, 3, 255),
        ],
    );
    let second_source = frame(
        3,
        2,
        &[
            Rgba8::new(255, 255, 255, 255),
            Rgba8::new(0, 0, 0, 255),
            Rgba8::new(128, 128, 128, 255),
            Rgba8::new(54, 183, 19, 255),
            Rgba8::new(19, 183, 54, 255),
            Rgba8::new(0, 128, 0, 255),
        ],
    );
    let sources = [
        (SourceId::new(1), &first_source),
        (SourceId::new(2), &second_source),
    ];
    let first = execute_sources(&plan, &sources);
    let second = execute_sources(&plan, &sources);
    assert_eq!(first, second);
    assert_eq!(first.pixels(), second.pixels());
}

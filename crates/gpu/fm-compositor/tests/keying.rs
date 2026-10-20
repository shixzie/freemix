//! Exact-pixel CPU-oracle coverage for auto key sampling, green/blue despill,
//! matte clip/choke refinement, and key/fill layers.
//!
//! Expected values are hand-computed from the integer formulas documented in
//! `crates/gpu/fm-compositor/src/keying.rs`. Chroma uses
//! `cb = (32896 - 29 r - 99 g + 128 b) / 256` and
//! `cr = (32896 + 128 r - 116 g - 12 b) / 256`; premultiplication and alpha
//! scaling use `(channel * alpha + 127) / 255`.

use fm_compositor::{
    AlphaMode, AutoKey, AutoKeyError, AutoKeyOptions, AutoKeyRegion, Backing, ChromaKey,
    CompositionPlan, CpuExecutionError, CpuSourceFrame, CropRect, Despill, Key, KeyFill,
    KeyRefinement, KeyRefinementError, LumaKey, MAX_AUTO_KEY_SAMPLES, OutputTarget, PlanError,
    Rgba8, Rotation, Scene, SourceId, SourceLayer, Transform, chroma_cb_cr, compile_scene,
    execute_cpu, sample_auto_key,
};
use fm_video::ImageFrame;

const GREEN: Rgba8 = Rgba8::new(40, 200, 60, 255);
const GREEN_LIGHT: Rgba8 = Rgba8::new(44, 190, 64, 255);
const GREEN_DARK: Rgba8 = Rgba8::new(36, 210, 56, 255);
const RED: Rgba8 = Rgba8::new(230, 40, 40, 255);
const SKIN: Rgba8 = Rgba8::new(200, 150, 120, 255);
const EDGE: Rgba8 = Rgba8::new(60, 180, 70, 255);
const BLUE: Rgba8 = Rgba8::new(20, 60, 200, 255);
const CLEAR: Rgba8 = Rgba8::new(0, 0, 0, 0);

fn frame(width: u32, height: u32, pixels: &[Rgba8]) -> ImageFrame {
    assert_eq!(pixels.len(), (width * height) as usize);
    ImageFrame::new(
        width,
        height,
        usize::try_from(width).unwrap() * 4,
        pixels.iter().flat_map(|pixel| pixel.to_bytes()).collect(),
    )
    .unwrap()
}

fn uniform(width: u32, height: u32, pixel: Rgba8) -> ImageFrame {
    frame(width, height, &vec![pixel; (width * height) as usize])
}

fn gray(value: u8) -> Rgba8 {
    Rgba8::new(value, value, value, 255)
}

fn pixels(frame: &ImageFrame) -> Vec<Rgba8> {
    (0..frame.height())
        .flat_map(|y| (0..frame.width()).map(move |x| frame.pixel(x, y).unwrap()))
        .collect()
}

fn layer(width: u32, height: u32) -> SourceLayer {
    SourceLayer::new(
        SourceId::new(1),
        0,
        Transform::new(0, 0, width, height, Rotation::Deg0),
    )
}

fn compile(width: u32, height: u32, layer: SourceLayer) -> Result<CompositionPlan, PlanError> {
    let mut scene = Scene::new(width, height, CLEAR).unwrap();
    scene.push_layer(layer);
    compile_scene(&scene, OutputTarget::Program).map(|(plan, _)| plan)
}

fn run(plan: &CompositionPlan, sources: &[(u64, &ImageFrame)]) -> ImageFrame {
    try_run(plan, sources).unwrap()
}

fn try_run(
    plan: &CompositionPlan,
    sources: &[(u64, &ImageFrame)],
) -> Result<ImageFrame, CpuExecutionError> {
    let bindings = sources
        .iter()
        .map(|(id, frame)| CpuSourceFrame::new(SourceId::new(*id), frame))
        .collect::<Vec<_>>();
    execute_cpu(plan, &bindings)
}

/// 6x4 green-screen plate. The one-pixel border holds ten `GREEN`, two of
/// each lighter/darker variant, and two red outliers; the interior holds
/// foreground, an edge-spill pixel, and one unsampled backing pixel.
fn green_plate() -> ImageFrame {
    frame(
        6,
        4,
        &[
            GREEN,
            GREEN_LIGHT,
            GREEN,
            GREEN,
            GREEN,
            RED, //
            GREEN_DARK,
            SKIN,
            EDGE,
            GREEN,
            SKIN,
            GREEN, //
            GREEN,
            SKIN,
            SKIN,
            EDGE,
            SKIN,
            GREEN_DARK, //
            RED,
            GREEN,
            GREEN,
            GREEN,
            GREEN_LIGHT,
            GREEN, //
        ],
    )
}

#[test]
fn chroma_coordinates_follow_the_documented_fixed_point_formula() {
    assert_eq!(chroma_cb_cr(GREEN), (76, 55));
    assert_eq!(chroma_cb_cr(GREEN_LIGHT), (82, 61));
    assert_eq!(chroma_cb_cr(GREEN_DARK), (71, 48));
    assert_eq!(chroma_cb_cr(RED), (106, 223));
    assert_eq!(chroma_cb_cr(BLUE), (203, 101));
    assert_eq!(chroma_cb_cr(gray(128)), (128, 128));
    // Extremes stay within range: pure blue clamps 256 to 255, pure yellow
    // reaches the minimum Cb of 1.
    assert_eq!(chroma_cb_cr(Rgba8::new(0, 0, 255, 255)).0, 255);
    assert_eq!(chroma_cb_cr(Rgba8::new(255, 255, 0, 255)).0, 1);
}

#[test]
fn green_border_auto_key_rejects_outliers_and_derives_exact_parameters() {
    let plate = green_plate();
    let auto = sample_auto_key(
        &plate,
        &AutoKeyOptions::new(AutoKeyRegion::Border { thickness: 1 }),
    )
    .unwrap();
    // Cb histogram 71x2, 76x10, 82x2, 106x2 -> lower median 76; Cr -> 55.
    // Red outliers are 30/168 away and do not agree; 14 of 16 do.
    // Distances from (40, 200, 60): 0 x10, 10 x4 -> p95 is 10, tolerance 18.
    assert_eq!(
        auto,
        AutoKey {
            backing: Backing::Green,
            color: GREEN,
            median_cb: 76,
            median_cr: 55,
            samples: 16,
            agreeing: 14,
            key: ChromaKey::new(GREEN, 18, 32, 0),
            despill: Despill::new(Backing::Green, 255),
        }
    );

    let plan = compile(
        6,
        4,
        layer(6, 4)
            .with_key(auto.layer_key())
            .with_key_refinement(auto.refinement()),
    )
    .unwrap();
    // EDGE: distance 20 -> alpha (2 * 255 + 16) / 32 = 16; despill limit
    // (60 + 70 + 1) / 2 = 65 pulls green 180 -> 65; premultiplied by 16 that
    // gives (4, 4, 4, 16). RED and SKIN are below their despill limits.
    let edge = Rgba8::new(4, 4, 4, 16);
    assert_eq!(
        pixels(&run(&plan, &[(1, &plate)])),
        [
            CLEAR, CLEAR, CLEAR, CLEAR, CLEAR, RED, //
            CLEAR, SKIN, edge, CLEAR, SKIN, CLEAR, //
            CLEAR, SKIN, SKIN, edge, SKIN, CLEAR, //
            RED, CLEAR, CLEAR, CLEAR, CLEAR, CLEAR, //
        ]
    );
}

#[test]
fn blue_rect_auto_key_uses_only_the_region_and_custom_options() {
    let mut pixels_in = vec![SKIN; 12];
    for y in 1..3 {
        for x in 1..4 {
            pixels_in[y * 4 + x] = BLUE;
        }
    }
    let plate = frame(4, 3, &pixels_in);
    let mut options = AutoKeyOptions::new(AutoKeyRegion::Rect(CropRect::new(1, 1, 3, 2)));
    options.tolerance_margin = 4;
    options.softness = 10;
    options.spill = 7;
    options.despill_strength = 128;
    let auto = sample_auto_key(&plate, &options).unwrap();
    assert_eq!(
        auto,
        AutoKey {
            backing: Backing::Blue,
            color: BLUE,
            median_cb: 203,
            median_cr: 101,
            samples: 6,
            agreeing: 6,
            key: ChromaKey::new(BLUE, 4, 10, 7),
            despill: Despill::new(Backing::Blue, 128),
        }
    );
    assert_eq!(
        auto.layer_key(),
        Key::Chroma(ChromaKey::new(BLUE, 4, 10, 7))
    );
}

#[test]
fn auto_key_reports_typed_errors() {
    let plate = green_plate();
    assert_eq!(
        sample_auto_key(
            &plate,
            &AutoKeyOptions::new(AutoKeyRegion::Border { thickness: 0 })
        ),
        Err(AutoKeyError::EmptyRegion)
    );
    assert_eq!(
        sample_auto_key(
            &plate,
            &AutoKeyOptions::new(AutoKeyRegion::Rect(CropRect::new(0, 0, 0, 1)))
        ),
        Err(AutoKeyError::EmptyRegion)
    );
    let outside = CropRect::new(4, 2, 3, 2);
    assert_eq!(
        sample_auto_key(&plate, &AutoKeyOptions::new(AutoKeyRegion::Rect(outside))),
        Err(AutoKeyError::RegionOutOfBounds {
            region: outside,
            width: 6,
            height: 4,
        })
    );
    let border = AutoKeyOptions::new(AutoKeyRegion::Border { thickness: 1 });
    assert_eq!(
        sample_auto_key(&uniform(3, 3, gray(128)), &border),
        Err(AutoKeyError::Unsaturated {
            saturation: 0,
            minimum: 24,
        })
    );
    assert_eq!(
        sample_auto_key(&uniform(3, 3, RED), &border),
        Err(AutoKeyError::UnsupportedBacking { cb: 106, cr: 223 })
    );
    // Two samples: the lower medians come from GREEN, RED disagrees, and one
    // of two is not a strict majority.
    assert_eq!(
        sample_auto_key(&frame(2, 1, &[GREEN, RED]), &border),
        Err(AutoKeyError::NoConsensus {
            agreeing: 1,
            samples: 2,
        })
    );
}

#[test]
fn auto_key_sampling_is_bounded_by_deterministic_stride() {
    // 300x300 = 90000 pixels -> stride ceil(90000 / 65536) = 2 -> 45000 reads.
    let auto = sample_auto_key(
        &uniform(300, 300, GREEN),
        &AutoKeyOptions::new(AutoKeyRegion::Rect(CropRect::new(0, 0, 300, 300))),
    )
    .unwrap();
    assert_eq!((auto.samples, auto.agreeing), (45_000, 45_000));

    // 512x512 = 262144 -> stride 4 -> exactly the cap. A border thicker than
    // half the frame covers every pixel once.
    let auto = sample_auto_key(
        &uniform(512, 512, GREEN),
        &AutoKeyOptions::new(AutoKeyRegion::Border { thickness: 400 }),
    )
    .unwrap();
    assert_eq!(auto.samples as usize, MAX_AUTO_KEY_SAMPLES);
    assert_eq!(auto.key, ChromaKey::new(GREEN, 8, 32, 0));
}

#[test]
fn despill_formula_is_exact_for_green_and_blue() {
    let green = Despill::new(Backing::Green, 255);
    // limit (60 + 70 + 1) / 2 = 65, excess 115 -> green 65.
    assert_eq!(green.apply(EDGE), Rgba8::new(60, 65, 70, 255));
    // strength 128: reduction (115 * 128 + 127) / 255 = 58 -> green 122.
    assert_eq!(
        Despill::new(Backing::Green, 128).apply(EDGE),
        Rgba8::new(60, 122, 70, 255)
    );
    assert_eq!(Despill::new(Backing::Green, 0).apply(EDGE), EDGE);
    // Below the limit nothing changes, and alpha is never touched.
    assert_eq!(green.apply(SKIN), SKIN);
    assert_eq!(
        green.apply(Rgba8::new(10, 90, 30, 90)),
        Rgba8::new(10, 20, 30, 90)
    );

    let blue = Despill::new(Backing::Blue, 255);
    // limit (100 + 120 + 1) / 2 = 110 -> blue 110; green is untouched.
    assert_eq!(
        blue.apply(Rgba8::new(100, 120, 250, 255)),
        Rgba8::new(100, 120, 110, 255)
    );
    // strength 64: reduction (140 * 64 + 127) / 255 = 35 -> blue 215.
    assert_eq!(
        Despill::new(Backing::Blue, 64).apply(Rgba8::new(100, 120, 250, 255)),
        Rgba8::new(100, 120, 215, 255)
    );
}

#[test]
fn identity_refinement_matches_the_legacy_chroma_path() {
    let key = Key::Chroma(ChromaKey::new(Rgba8::new(100, 255, 150, 255), 0, 255, 255));
    let source = frame(1, 1, &[Rgba8::new(100, 55, 150, 255)]);
    let legacy = compile(1, 1, layer(1, 1).with_key(key)).unwrap();
    let refined = compile(
        1,
        1,
        layer(1, 1)
            .with_key(key)
            .with_key_refinement(KeyRefinement::new()),
    )
    .unwrap();
    let expected = [Rgba8::new(78, 59, 118, 200)];
    assert_eq!(pixels(&run(&legacy, &[(1, &source)])), expected);
    assert_eq!(pixels(&run(&refined, &[(1, &source)])), expected);
}

#[test]
fn clip_remaps_the_matte_with_round_half_up() {
    // Linear luma key: a gray pixel's alpha equals its value.
    let linear = Key::Luma(LumaKey::new(0, 255, false));
    let source = frame(6, 1, &[0, 50, 51, 100, 200, 255].map(gray));
    let plan = compile(
        6,
        1,
        layer(6, 1)
            .with_key(linear)
            .with_key_refinement(KeyRefinement::new().with_clip(50, 200)),
    )
    .unwrap();
    // 51 -> (1 * 255 + 75) / 150 = 2; 100 -> (50 * 255 + 75) / 150 = 85.
    assert_eq!(
        pixels(&run(&plan, &[(1, &source)])),
        [
            CLEAR,
            CLEAR,
            Rgba8::new(0, 0, 0, 2),
            Rgba8::new(33, 33, 33, 85),
            gray(200),
            gray(255),
        ]
    );
}

#[test]
fn choke_erodes_and_spreads_with_frame_clamped_windows() {
    let linear = Key::Luma(LumaKey::new(0, 255, false));
    let white = gray(255);
    let black = gray(0);

    // Columns 0..=2 white, column 3 black. Out-of-frame pixels are ignored,
    // so only column 2 (which sees column 3) erodes.
    let wall = frame(4, 3, &[white, white, white, black].repeat(3));
    let choked = compile(
        4,
        3,
        layer(4, 3)
            .with_key(linear)
            .with_key_refinement(KeyRefinement::new().with_choke(1)),
    )
    .unwrap();
    assert_eq!(
        pixels(&run(&choked, &[(1, &wall)])),
        [white, white, CLEAR, CLEAR].repeat(3)
    );

    // One white pixel at (1, 1) spreads to the clamped 3x3 window.
    let mut dot = vec![black; 12];
    dot[5] = white;
    let dot = frame(4, 3, &dot);
    let spread = compile(
        4,
        3,
        layer(4, 3)
            .with_key(linear)
            .with_key_refinement(KeyRefinement::new().with_choke(-1)),
    )
    .unwrap();
    assert_eq!(
        pixels(&run(&spread, &[(1, &dot)])),
        [
            black, black, black, CLEAR, //
            black, white, black, CLEAR, //
            black, black, black, CLEAR, //
        ]
    );
}

#[test]
fn key_fill_takes_alpha_from_key_luma_in_straight_and_premultiplied_modes() {
    let fill = frame(
        3,
        1,
        &[
            Rgba8::new(200, 100, 50, 255),
            Rgba8::new(10, 20, 30, 255),
            gray(255),
        ],
    );
    // Key alpha is ignored: the middle key pixel is transparent but gray 128.
    let key = frame(3, 1, &[gray(255), Rgba8::new(128, 128, 128, 0), gray(0)]);
    let plan = compile(
        3,
        1,
        layer(3, 1).with_key_fill(KeyFill::linear(SourceId::new(2))),
    )
    .unwrap();
    assert_eq!(
        plan.layers()[0].key_fill(),
        Some(KeyFill::new(SourceId::new(2), LumaKey::new(0, 255, false)))
    );
    assert_eq!(
        pixels(&run(&plan, &[(1, &fill), (2, &key)])),
        [
            Rgba8::new(200, 100, 50, 255),
            Rgba8::new(5, 10, 15, 128),
            CLEAR,
        ]
    );

    // Premultiplied fill scales every channel; a red key has luma
    // (54 * 255 + 128) / 256 = 54.
    let fill = frame(
        2,
        1,
        &[Rgba8::new(100, 50, 20, 200), Rgba8::new(255, 255, 255, 255)],
    );
    let key = frame(2, 1, &[gray(128), Rgba8::new(255, 0, 0, 255)]);
    let plan = compile(
        2,
        1,
        layer(2, 1)
            .with_alpha_mode(AlphaMode::Premultiplied)
            .with_key_fill(KeyFill::linear(SourceId::new(2))),
    )
    .unwrap();
    assert_eq!(
        pixels(&run(&plan, &[(1, &fill), (2, &key)])),
        [Rgba8::new(50, 25, 10, 100), Rgba8::new(54, 54, 54, 54)]
    );
}

#[test]
fn key_fill_crops_both_inputs_and_accepts_refinement() {
    let fill = uniform(3, 1, gray(255));
    let key = frame(3, 1, &[gray(0), gray(255), gray(100)]);
    let plan = compile(
        2,
        1,
        layer(2, 1)
            .with_crop(CropRect::new(1, 0, 2, 1))
            .with_key_fill(KeyFill::linear(SourceId::new(2)))
            .with_key_refinement(KeyRefinement::new().with_clip(50, 200)),
    )
    .unwrap();
    // Cropped key [255, 100] clipped to [255, 85].
    assert_eq!(
        pixels(&run(&plan, &[(1, &fill), (2, &key)])),
        [gray(255), Rgba8::new(85, 85, 85, 85)]
    );

    // An inverted, thresholded matte: luma 100 -> ramp (100 - 50) * 255 / 100
    // rounded = 128 -> inverted 127.
    let plan = compile(
        1,
        1,
        layer(1, 1).with_key_fill(KeyFill::new(SourceId::new(2), LumaKey::new(50, 100, true))),
    )
    .unwrap();
    assert_eq!(
        pixels(&run(
            &plan,
            &[
                (1, &uniform(1, 1, gray(255))),
                (2, &uniform(1, 1, gray(100)))
            ]
        )),
        [Rgba8::new(127, 127, 127, 127)]
    );
}

#[test]
fn key_fill_execution_errors_are_explicit() {
    let plan = compile(
        3,
        1,
        layer(3, 1).with_key_fill(KeyFill::linear(SourceId::new(2))),
    )
    .unwrap();
    let fill = uniform(3, 1, gray(255));
    assert!(matches!(
        try_run(&plan, &[(1, &fill)]),
        Err(CpuExecutionError::MissingSource(SourceId(2)))
    ));
    let short_key = uniform(2, 1, gray(255));
    assert!(matches!(
        try_run(&plan, &[(1, &fill), (2, &short_key)]),
        Err(CpuExecutionError::KeyFillDimensionMismatch {
            layer: 0,
            fill_width: 3,
            fill_height: 1,
            key_width: 2,
            key_height: 1,
        })
    ));
}

#[test]
fn planner_rejects_invalid_key_combinations() {
    let chroma = Key::Chroma(ChromaKey::new(GREEN, 0, 0, 0));
    assert_eq!(
        compile(
            1,
            1,
            layer(1, 1)
                .with_key(chroma)
                .with_key_fill(KeyFill::linear(SourceId::new(2)))
        ),
        Err(PlanError::ConflictingKeys { layer: 0 })
    );
    assert_eq!(
        compile(1, 1, layer(1, 1).with_key_refinement(KeyRefinement::new())),
        Err(PlanError::KeyRefinementWithoutKey { layer: 0 })
    );
    assert_eq!(
        compile(
            1,
            1,
            layer(1, 1)
                .with_key(chroma)
                .with_key_refinement(KeyRefinement::new().with_clip(100, 100))
        ),
        Err(PlanError::InvalidKeyRefinement {
            layer: 0,
            reason: KeyRefinementError::EmptyClipRange {
                clip_black: 100,
                clip_white: 100,
            },
        })
    );
    assert_eq!(
        compile(
            1,
            1,
            layer(1, 1)
                .with_key(chroma)
                .with_key_refinement(KeyRefinement::new().with_choke(-9))
        ),
        Err(PlanError::InvalidKeyRefinement {
            layer: 0,
            reason: KeyRefinementError::ChokeOutOfRange {
                choke: -9,
                maximum: 8,
            },
        })
    );
    // The maximum radius itself is accepted.
    assert!(
        compile(
            1,
            1,
            layer(1, 1)
                .with_key(chroma)
                .with_key_refinement(KeyRefinement::new().with_choke(8))
        )
        .is_ok()
    );
}

#[test]
fn edge_softness_averages_the_matte_over_frame_clamped_windows() {
    let linear = Key::Luma(LumaKey::new(0, 255, false));
    let source = frame(3, 1, &[gray(255), gray(0), gray(0)]);
    let plan = compile(
        3,
        1,
        layer(3, 1)
            .with_key(linear)
            .with_key_refinement(KeyRefinement::new().with_edge_softness(1)),
    )
    .unwrap();
    // Windows: {255, 0} -> (255 + 1) / 2 = 128; {255, 0, 0} -> (255 + 1) / 3
    // = 85; {0, 0} -> 0.
    assert_eq!(
        pixels(&run(&plan, &[(1, &source)])),
        [
            Rgba8::new(128, 128, 128, 128),
            Rgba8::new(0, 0, 0, 85),
            CLEAR
        ]
    );

    // Softness follows choke: the spread single white pixel is then
    // averaged.
    let source = frame(3, 1, &[gray(0), gray(255), gray(0)]);
    let plan = compile(
        3,
        1,
        layer(3, 1)
            .with_key(linear)
            .with_key_refinement(KeyRefinement::new().with_choke(1).with_edge_softness(1)),
    )
    .unwrap();
    // Choke 1 erodes everything to 0.
    assert_eq!(pixels(&run(&plan, &[(1, &source)])), [CLEAR; 3]);

    assert_eq!(
        compile(
            1,
            1,
            layer(1, 1)
                .with_key(linear)
                .with_key_refinement(KeyRefinement::new().with_edge_softness(9))
        ),
        Err(PlanError::InvalidKeyRefinement {
            layer: 0,
            reason: KeyRefinementError::EdgeSoftnessOutOfRange {
                radius: 9,
                maximum: 8,
            },
        })
    );
}

#[test]
fn alpha_channel_key_fill_reads_the_key_source_alpha() {
    let fill = uniform(3, 1, gray(255));
    // RGB is ignored: only the stored alpha drives the matte.
    let key = frame(
        3,
        1,
        &[
            Rgba8::new(0, 0, 0, 200),
            Rgba8::new(255, 255, 255, 0),
            Rgba8::new(9, 9, 9, 255),
        ],
    );
    let plan = compile(
        3,
        1,
        layer(3, 1).with_key_fill(
            KeyFill::linear(SourceId::new(2)).with_channel(fm_compositor::KeyChannel::Alpha),
        ),
    )
    .unwrap();
    assert_eq!(
        pixels(&run(&plan, &[(1, &fill), (2, &key)])),
        [Rgba8::new(200, 200, 200, 200), CLEAR, gray(255)]
    );

    // Gain and invert apply to the alpha too: threshold 100, softness 100,
    // inverted -> alpha 200 keys to 255 - 255 = 0, alpha 0 to 255.
    let plan = compile(
        3,
        1,
        layer(3, 1).with_key_fill(
            KeyFill::new(SourceId::new(2), LumaKey::new(100, 100, true))
                .with_channel(fm_compositor::KeyChannel::Alpha),
        ),
    )
    .unwrap();
    assert_eq!(
        pixels(&run(&plan, &[(1, &fill), (2, &key)])),
        [CLEAR, gray(255), CLEAR]
    );
}

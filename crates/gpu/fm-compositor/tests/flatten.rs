//! SW-018 flatten layers: the flattened schedule must reproduce the
//! unflattened nested composition pixel for pixel on the CPU reference path.
//!
//! The oracle here is written independently of `plan_flatten`: it compiles
//! each scene with `compile_scene`, recursively renders every nested scene a
//! layer (or key/fill key source) names into its own canvas, binds the result
//! as a source, and calls `execute_cpu`. It re-renders a shared scene for
//! every consumer, which is exactly the work flattening is meant to remove.

use std::collections::BTreeMap;
use std::num::NonZeroU128;

use fm_compositor::{
    AlphaMode, ChromaKey, CpuSourceFrame, CropRect, Effect, FlattenError, FlattenExecutionError,
    FlattenLimits, FlattenMode, FlattenOutput, FlattenPlan, FlattenStep, HoistedLayer, Key,
    KeyFill, LumaKey, OutputInclusion, OutputTarget, RectMask, Rgba8, Rotation, Scene, SceneGraph,
    SourceId, SourceLayer, Transform, compile_scene, execute_cpu, execute_flatten_cpu,
    execute_flatten_cpu_frame, plan_flatten,
};
use fm_frame::{
    ClockDomainId, CpuVideoFrame, CpuVideoPayload, CpuVideoPlane, MediaTimestamp, MediaTiming,
    NormalizedDuration, NormalizedTimestamp, OriginalTimestamp, PixelFormat, SequenceNumber,
    TimeBase, VideoDimensions,
};
use fm_video::ImageFrame;

const L1: SourceId = SourceId::new(1);
const L2: SourceId = SourceId::new(2);
const L3: SourceId = SourceId::new(3);
const L4: SourceId = SourceId::new(4);
const TRANSPARENT: Rgba8 = Rgba8::new(0, 0, 0, 0);

fn id(value: u64) -> SourceId {
    SourceId::new(value)
}

fn transform(x: i32, y: i32, width: u32, height: u32) -> Transform {
    Transform::new(x, y, width, height, Rotation::Deg0)
}

fn rotated(x: i32, y: i32, width: u32, height: u32, rotation: Rotation) -> Transform {
    Transform::new(x, y, width, height, rotation)
}

fn image(width: u32, height: u32, pixel: impl Fn(u32, u32) -> Rgba8) -> ImageFrame {
    let mut bytes = Vec::new();
    for y in 0..height {
        for x in 0..width {
            bytes.extend_from_slice(&pixel(x, y).to_bytes());
        }
    }
    ImageFrame::new(width, height, width as usize * 4, bytes).unwrap()
}

/// Straight-alpha pattern covering transparent, partial, and opaque alpha.
fn straight(width: u32, height: u32, seed: u32) -> ImageFrame {
    image(width, height, |x, y| pattern(x, y, seed))
}

#[allow(clippy::cast_possible_truncation)]
fn pattern(x: u32, y: u32, seed: u32) -> Rgba8 {
    const ALPHAS: [u8; 5] = [0, 64, 128, 200, 255];
    Rgba8::new(
        ((x * 37 + y * 11 + seed * 53) % 256) as u8,
        ((x * 13 + y * 41 + seed * 7) % 256) as u8,
        ((x * 5 + y * 29 + seed * 101) % 256) as u8,
        ALPHAS[((x + y * 3 + seed) % 5) as usize],
    )
}

#[allow(clippy::cast_possible_truncation)]
fn premultiplied(width: u32, height: u32, seed: u32) -> ImageFrame {
    image(width, height, |x, y| {
        let pixel = pattern(x, y, seed);
        let scale = |channel: u8| ((u16::from(channel) * u16::from(pixel.a) + 127) / 255) as u8;
        Rgba8::new(scale(pixel.r), scale(pixel.g), scale(pixel.b), pixel.a)
    })
}

/// Opaque frame where every third diagonal is near key green.
fn green_screen(width: u32, height: u32) -> ImageFrame {
    image(width, height, |x, y| {
        if (x + y) % 3 == 0 {
            Rgba8::new(20, 240, 30, 255)
        } else {
            let pixel = pattern(x, y, 9);
            Rgba8::new(pixel.r, pixel.g, pixel.b, 255)
        }
    })
}

fn leaves() -> Vec<(SourceId, ImageFrame)> {
    vec![
        (L1, straight(8, 6, 1)),
        (L2, premultiplied(6, 6, 2)),
        (L3, green_screen(6, 5)),
        (L4, straight(6, 4, 4)),
    ]
}

fn bindings(leaves: &[(SourceId, ImageFrame)]) -> Vec<CpuSourceFrame<'_>> {
    leaves
        .iter()
        .map(|(source, frame)| CpuSourceFrame::new(*source, frame))
        .collect()
}

fn scene(width: u32, height: u32, background: Rgba8, layers: Vec<SourceLayer>) -> Scene {
    let mut scene = Scene::new(width, height, background).unwrap();
    for layer in layers {
        scene.push_layer(layer);
    }
    scene
}

fn premul(source: SourceId, z: i32, transform: Transform) -> SourceLayer {
    SourceLayer::new(source, z, transform).with_alpha_mode(AlphaMode::Premultiplied)
}

fn assert_nontrivial(frame: &ImageFrame) {
    let mut distinct = Vec::new();
    for y in 0..frame.height() {
        for x in 0..frame.width() {
            let pixel = frame.pixel(x, y).unwrap();
            if !distinct.contains(&pixel) {
                distinct.push(pixel);
            }
        }
    }
    assert!(
        distinct.len() >= 8,
        "only {} distinct pixels",
        distinct.len()
    );
}

#[derive(Default)]
struct Fixture {
    graph: SceneGraph,
    scenes: BTreeMap<SourceId, Scene>,
}

impl Fixture {
    fn add(&mut self, scene_id: SourceId, scene: Scene, mode: FlattenMode) {
        self.graph
            .insert_with_mode(scene_id, scene.clone(), mode)
            .unwrap();
        self.scenes.insert(scene_id, scene);
    }

    /// Unflattened reference: every nested reference re-rendered.
    fn oracle(
        &self,
        root: SourceId,
        target: OutputTarget,
        leaves: &[(SourceId, ImageFrame)],
        renders: &mut Vec<SourceId>,
    ) -> ImageFrame {
        let (plan, _) = compile_scene(&self.scenes[&root], target).unwrap();
        let mut nested: Vec<(SourceId, ImageFrame)> = Vec::new();
        for layer in plan.layers() {
            let key_source = layer.key_fill().map(|fill| fill.key_source);
            for source in std::iter::once(layer.source()).chain(key_source) {
                if self.scenes.contains_key(&source)
                    && !nested.iter().any(|(bound, _)| *bound == source)
                {
                    renders.push(source);
                    let frame = self.oracle(source, target, leaves, renders);
                    nested.push((source, frame));
                }
            }
        }
        let mut all = bindings(leaves);
        all.extend(
            nested
                .iter()
                .map(|(source, frame)| CpuSourceFrame::new(*source, frame)),
        );
        execute_cpu(&plan, &all).unwrap()
    }

    fn flatten(
        &self,
        root: SourceId,
        target: OutputTarget,
        leaves: &[(SourceId, ImageFrame)],
    ) -> (FlattenPlan, FlattenOutput) {
        let plan = plan_flatten(&self.graph, root, target, FlattenLimits::default()).unwrap();
        let output = execute_flatten_cpu(&plan, &bindings(leaves)).unwrap();
        (plan, output)
    }

    /// Flattens, checks pixel equality with the oracle, and checks that the
    /// executor's measured peak matches the plan.
    fn assert_equivalent(
        &self,
        root: SourceId,
        target: OutputTarget,
    ) -> (FlattenPlan, FlattenOutput, Vec<SourceId>) {
        let leaves = leaves();
        let mut oracle_renders = Vec::new();
        let expected = self.oracle(root, target, &leaves, &mut oracle_renders);
        let (plan, output) = self.flatten(root, target, &leaves);
        assert_eq!(
            output.frame, expected,
            "flattened pixels differ for {target:?}"
        );
        assert_nontrivial(&output.frame);
        assert_eq!(output.peak_live_bytes, plan.peak_live_bytes());
        (plan, output, oracle_renders)
    }
}

const ROOT: SourceId = SourceId::new(100);
const INNER: SourceId = SourceId::new(200);
const MID: SourceId = SourceId::new(300);
const WRAP: SourceId = SourceId::new(400);

/// Root -> {Mid -> Inner x2, Inner, Wrap(hoistable)} with crops, masks,
/// chroma/luma keys, effects, rotations, opacity, straight and premultiplied
/// alpha, and an overlay routed to Preview only.
fn nested_fixture() -> Fixture {
    let mut fixture = Fixture::default();
    fixture.add(
        INNER,
        scene(
            12,
            10,
            Rgba8::new(10, 20, 30, 128),
            vec![
                SourceLayer::new(L1, 0, transform(1, 1, 9, 7))
                    .with_crop(CropRect::new(1, 1, 6, 4))
                    .with_mask(RectMask::new(1, 0, 4, 3))
                    .with_opacity(200)
                    .with_effect(Effect::new("saturation", vec![-40])),
                SourceLayer::new(L3, 1, rotated(3, 2, 8, 6, Rotation::Deg180)).with_key(
                    Key::Chroma(ChromaKey::new(Rgba8::new(0, 255, 0, 255), 40, 30, 120)),
                ),
                premul(L2, 2, rotated(6, 0, 5, 6, Rotation::Deg90))
                    .with_opacity(230)
                    .with_mask(RectMask::new(2, 2, 2, 2).inverted(true)),
                SourceLayer::new(L1, 3, transform(0, 0, 4, 4)).as_overlay(OutputInclusion::PREVIEW),
            ],
        ),
        FlattenMode::Auto,
    );
    fixture.add(
        MID,
        scene(
            16,
            12,
            TRANSPARENT,
            vec![
                premul(INNER, 0, transform(2, 1, 10, 8))
                    .with_key(Key::Luma(LumaKey::new(40, 20, false))),
                premul(INNER, 1, transform(4, 2, 12, 10)).with_opacity(180),
                SourceLayer::new(L1, 2, transform(0, 8, 6, 4)),
            ],
        ),
        FlattenMode::Auto,
    );
    fixture.add(
        WRAP,
        scene(
            10,
            8,
            TRANSPARENT,
            vec![
                SourceLayer::new(L3, 0, transform(1, 1, 6, 5))
                    .with_crop(CropRect::new(0, 0, 5, 4))
                    .with_key(Key::Chroma(ChromaKey::new(
                        Rgba8::new(0, 255, 0, 255),
                        30,
                        0,
                        200,
                    )))
                    .with_opacity(210),
            ],
        ),
        FlattenMode::Auto,
    );
    fixture.add(
        ROOT,
        scene(
            24,
            18,
            Rgba8::new(0, 0, 40, 255),
            vec![
                premul(L2, 0, transform(0, 0, 24, 18)),
                premul(MID, 1, transform(3, 2, 16, 12)),
                premul(INNER, 2, rotated(12, 6, 8, 10, Rotation::Deg270))
                    .with_crop(CropRect::new(1, 1, 10, 8))
                    .with_mask(RectMask::new(0, 0, 8, 6)),
                premul(WRAP, 3, transform(13, 9, 10, 8)),
            ],
        ),
        FlattenMode::Auto,
    );
    fixture
}

#[test]
fn nested_scenes_flatten_pixel_identically_for_every_target() {
    let fixture = nested_fixture();
    for target in [
        OutputTarget::Program,
        OutputTarget::Preview,
        OutputTarget::Operator,
    ] {
        let (plan, output, oracle_renders) = fixture.assert_equivalent(ROOT, target);
        // Inner is shared by Mid and Root: the oracle renders it twice, the
        // flattened schedule once.
        assert_eq!(
            oracle_renders
                .iter()
                .filter(|scene| **scene == INNER)
                .count(),
            2
        );
        assert_eq!(output.renders, vec![INNER, MID]);
        assert_eq!(
            plan.hoisted(),
            &[HoistedLayer {
                consumer: ROOT,
                layer: 3,
                bypassed: WRAP,
            }]
        );
        assert_eq!(
            plan.steps(),
            &[
                FlattenStep::Render {
                    scene: INNER,
                    slot: 0
                },
                FlattenStep::Render {
                    scene: MID,
                    slot: 1
                },
                FlattenStep::RenderRoot,
                FlattenStep::Release {
                    scene: MID,
                    slot: 1
                },
                FlattenStep::Release {
                    scene: INNER,
                    slot: 0
                },
            ]
        );
        // Inner 12x10 and Mid 16x12 are alive together while Mid renders.
        assert_eq!(plan.peak_live_bytes(), 12 * 10 * 4 + 16 * 12 * 4);
        assert_eq!(plan.peak_live_intermediates(), 2);
        assert_eq!(plan.output_bytes(), 24 * 18 * 4);
    }
}

#[test]
fn preview_overlay_inside_a_nested_scene_changes_only_preview() {
    let fixture = nested_fixture();
    let leaves = leaves();
    let (_, program) = fixture.flatten(ROOT, OutputTarget::Program, &leaves);
    let (_, preview) = fixture.flatten(ROOT, OutputTarget::Preview, &leaves);
    assert_ne!(program.frame, preview.frame);
}

#[test]
fn flatten_plans_are_deterministic_and_independent_of_insertion_order() {
    let fixture = nested_fixture();
    let mut reversed = SceneGraph::new();
    for (scene_id, scene) in fixture.scenes.iter().rev() {
        reversed.insert(*scene_id, scene.clone()).unwrap();
    }
    let first = plan_flatten(
        &fixture.graph,
        ROOT,
        OutputTarget::Program,
        FlattenLimits::default(),
    )
    .unwrap();
    let second = plan_flatten(
        &fixture.graph,
        ROOT,
        OutputTarget::Program,
        FlattenLimits::default(),
    )
    .unwrap();
    let third = plan_flatten(
        &reversed,
        ROOT,
        OutputTarget::Program,
        FlattenLimits::default(),
    )
    .unwrap();
    assert_eq!(first, second);
    assert_eq!(first, third);
}

#[test]
fn single_layer_wrapper_chain_is_hoisted_without_intermediates() {
    let wrapper_inner = id(401);
    let wrapper_outer = id(402);
    let mut fixture = Fixture::default();
    fixture.add(
        wrapper_inner,
        scene(
            10,
            8,
            TRANSPARENT,
            vec![
                SourceLayer::new(L1, 0, transform(2, 1, 6, 5))
                    .with_crop(CropRect::new(1, 0, 7, 6))
                    .with_mask(RectMask::new(0, 1, 5, 4))
                    .with_key(Key::Luma(LumaKey::new(30, 40, true)))
                    .with_effect(Effect::new("brightness_contrast", vec![10, 20]))
                    .with_opacity(128),
            ],
        ),
        FlattenMode::Auto,
    );
    fixture.add(
        wrapper_outer,
        scene(
            14,
            12,
            TRANSPARENT,
            vec![premul(wrapper_inner, 0, transform(3, 2, 10, 8))],
        ),
        FlattenMode::Auto,
    );
    fixture.add(
        ROOT,
        scene(
            20,
            16,
            Rgba8::new(50, 60, 70, 255),
            vec![
                premul(L2, 0, transform(0, 0, 20, 16)),
                premul(wrapper_outer, 1, transform(4, 3, 14, 12)),
            ],
        ),
        FlattenMode::Auto,
    );
    let (plan, output, oracle_renders) = fixture.assert_equivalent(ROOT, OutputTarget::Program);
    assert_eq!(oracle_renders, vec![wrapper_outer, wrapper_inner]);
    assert_eq!(output.renders, Vec::<SourceId>::new());
    assert_eq!(plan.intermediates().len(), 0);
    assert_eq!(plan.steps(), &[FlattenStep::RenderRoot]);
    assert_eq!(plan.peak_live_bytes(), 0);
    assert_eq!(
        plan.hoisted(),
        &[
            HoistedLayer {
                consumer: ROOT,
                layer: 1,
                bypassed: wrapper_outer,
            },
            HoistedLayer {
                consumer: ROOT,
                layer: 1,
                bypassed: wrapper_inner,
            },
        ]
    );
    let root_plan = plan.scene_plan(ROOT).unwrap();
    let hoisted = &root_plan.layers()[1];
    assert_eq!(hoisted.source(), L1);
    assert_eq!(hoisted.transform(), transform(4 + 3 + 2, 3 + 2 + 1, 6, 5));
    assert_eq!(hoisted.opacity(), 128);
    assert_eq!(hoisted.z(), 1);
}

const WRAPPER: SourceId = SourceId::new(410);

/// Root with a background layer and one layer drawing `wrapper`.
fn wrapper_fixture(wrapper: Scene, mode: FlattenMode, outer: SourceLayer) -> Fixture {
    let mut fixture = Fixture::default();
    fixture.add(WRAPPER, wrapper, mode);
    fixture.add(
        ROOT,
        scene(
            20,
            16,
            Rgba8::new(50, 60, 70, 255),
            vec![premul(L2, 0, transform(0, 0, 20, 16)), outer],
        ),
        FlattenMode::Auto,
    );
    fixture
}

fn single_layer_wrapper(inner: Transform) -> Scene {
    scene(
        10,
        8,
        TRANSPARENT,
        vec![
            SourceLayer::new(L1, 0, inner)
                .with_opacity(170)
                .with_mask(RectMask::new(1, 1, 5, 4)),
        ],
    )
}

fn plain_outer() -> SourceLayer {
    premul(WRAPPER, 1, transform(5, 4, 10, 8))
}

#[test]
#[allow(clippy::too_many_lines)]
fn hoisting_applies_only_when_it_is_exact_and_refusals_still_match() {
    let inside = transform(1, 1, 8, 6);
    let hoistable = wrapper_fixture(
        single_layer_wrapper(inside),
        FlattenMode::Auto,
        plain_outer(),
    );
    let (plan, _, _) = hoistable.assert_equivalent(ROOT, OutputTarget::Program);
    assert_eq!(plan.hoisted().len(), 1);
    assert_eq!(plan.intermediates().len(), 0);

    let overlapping = scene(
        10,
        8,
        TRANSPARENT,
        vec![
            SourceLayer::new(L1, 0, transform(0, 0, 8, 6)),
            premul(L2, 1, transform(2, 1, 8, 6)),
        ],
    );
    let refusals = [
        (
            "layer clipped by the wrapper canvas",
            single_layer_wrapper(transform(6, 4, 6, 5)),
            FlattenMode::Auto,
            plain_outer(),
        ),
        (
            "translucent consumer",
            single_layer_wrapper(inside),
            FlattenMode::Auto,
            plain_outer().with_opacity(200),
        ),
        (
            "straight-alpha consumer",
            single_layer_wrapper(inside),
            FlattenMode::Auto,
            SourceLayer::new(WRAPPER, 1, transform(5, 4, 10, 8)),
        ),
        (
            "rescaled consumer",
            single_layer_wrapper(inside),
            FlattenMode::Auto,
            premul(WRAPPER, 1, transform(0, 0, 20, 16)),
        ),
        (
            "rotated consumer",
            single_layer_wrapper(inside),
            FlattenMode::Auto,
            premul(WRAPPER, 1, rotated(5, 4, 10, 8, Rotation::Deg180)),
        ),
        (
            "cropped consumer",
            single_layer_wrapper(inside),
            FlattenMode::Auto,
            plain_outer().with_crop(CropRect::new(0, 0, 9, 8)),
        ),
        (
            "masked consumer",
            single_layer_wrapper(inside),
            FlattenMode::Auto,
            plain_outer().with_mask(RectMask::new(2, 2, 4, 4)),
        ),
        (
            "keyed consumer",
            single_layer_wrapper(inside),
            FlattenMode::Auto,
            plain_outer().with_key(Key::Luma(LumaKey::new(60, 10, false))),
        ),
        (
            "opaque wrapper background",
            scene(
                10,
                8,
                Rgba8::new(8, 8, 8, 64),
                vec![SourceLayer::new(L1, 0, inside)],
            ),
            FlattenMode::Auto,
            plain_outer(),
        ),
        (
            "deliberate precompose boundary",
            single_layer_wrapper(inside),
            FlattenMode::Precompose,
            plain_outer(),
        ),
        (
            "two overlapping translucent layers",
            overlapping,
            FlattenMode::Auto,
            plain_outer(),
        ),
        (
            "graded consumer",
            single_layer_wrapper(inside),
            FlattenMode::Auto,
            plain_outer().with_grade(std::sync::Arc::new(
                fm_grade::CompiledGrade::new(
                    &fm_grade::Grade {
                        saturation_permille: 0,
                        ..fm_grade::Grade::IDENTITY
                    },
                    None,
                )
                .unwrap(),
            )),
        ),
    ];
    for (case, wrapper, mode, outer) in refusals {
        let fixture = wrapper_fixture(wrapper, mode, outer);
        let (plan, output, _) = fixture.assert_equivalent(ROOT, OutputTarget::Program);
        assert!(plan.hoisted().is_empty(), "{case} was hoisted");
        assert_eq!(output.renders, vec![WRAPPER], "{case}");
        assert_eq!(plan.peak_live_bytes(), 10 * 8 * 4, "{case}");
    }
}

#[test]
fn inlining_overlapping_translucent_layers_would_not_be_exact() {
    // Integer premultiplied source-over is not associative, so the two
    // overlapping layers must be precomposed as a group; naively moving them
    // into the root changes pixels. This is why the planner refuses.
    let group = scene(
        10,
        8,
        TRANSPARENT,
        vec![
            SourceLayer::new(L1, 0, transform(0, 0, 8, 6)),
            premul(L2, 1, transform(2, 1, 8, 6)),
        ],
    );
    let fixture = wrapper_fixture(group, FlattenMode::Auto, plain_outer());
    let (_, output, _) = fixture.assert_equivalent(ROOT, OutputTarget::Program);

    let inlined = scene(
        20,
        16,
        Rgba8::new(50, 60, 70, 255),
        vec![
            premul(L2, 0, transform(0, 0, 20, 16)),
            SourceLayer::new(L1, 1, transform(5, 4, 8, 6)),
            premul(L2, 2, transform(7, 5, 8, 6)),
        ],
    );
    let (plan, _) = compile_scene(&inlined, OutputTarget::Program).unwrap();
    let leaves = leaves();
    let naive = execute_cpu(&plan, &bindings(&leaves)).unwrap();
    assert_ne!(naive, output.frame);
}

#[test]
fn shared_subscene_renders_once_and_reports_its_costs() {
    let shared = id(500);
    let first = id(501);
    let second = id(502);
    let mut fixture = Fixture::default();
    fixture.add(
        shared,
        scene(
            8,
            8,
            Rgba8::new(0, 30, 0, 90),
            vec![
                SourceLayer::new(L1, 0, transform(0, 0, 8, 6)),
                premul(L2, 1, transform(3, 3, 5, 5)).with_opacity(150),
            ],
        ),
        FlattenMode::Auto,
    );
    fixture.add(
        first,
        scene(
            12,
            12,
            TRANSPARENT,
            vec![
                premul(shared, 0, transform(0, 0, 8, 8)),
                SourceLayer::new(shared, 1, transform(4, 4, 6, 6)),
            ],
        ),
        FlattenMode::Auto,
    );
    fixture.add(
        second,
        scene(
            10,
            10,
            Rgba8::new(20, 0, 0, 255),
            vec![
                premul(shared, 0, transform(1, 1, 8, 8)),
                SourceLayer::new(L1, 1, transform(0, 6, 4, 3)),
            ],
        ),
        FlattenMode::Auto,
    );
    fixture.add(
        ROOT,
        scene(
            20,
            20,
            Rgba8::new(0, 0, 0, 255),
            vec![
                premul(first, 0, transform(0, 0, 12, 12)),
                premul(second, 1, transform(10, 10, 10, 10)),
                premul(shared, 2, transform(2, 12, 8, 8)).with_opacity(128),
            ],
        ),
        FlattenMode::Auto,
    );
    let (plan, output, oracle_renders) = fixture.assert_equivalent(ROOT, OutputTarget::Program);
    assert_eq!(
        oracle_renders
            .iter()
            .filter(|scene| **scene == shared)
            .count(),
        3
    );
    assert_eq!(
        output
            .renders
            .iter()
            .filter(|scene| **scene == shared)
            .count(),
        1
    );
    assert_eq!(output.renders.len(), 3);
    let cost = plan
        .intermediates()
        .iter()
        .find(|cost| cost.scene == shared)
        .unwrap();
    assert_eq!(cost.bytes, 8 * 8 * 4);
    assert_eq!(cost.consumer_scenes, 3);
    assert_eq!(cost.consumer_layers, 4);
    assert_eq!(cost.rescaled_uses, 1);
    assert_eq!(cost.straight_alpha_uses, 1);
    // The shared scene is released only after its last consumer, the root.
    let release = plan
        .steps()
        .iter()
        .position(|step| matches!(step, FlattenStep::Release { scene, .. } if *scene == shared))
        .unwrap();
    let root = plan
        .steps()
        .iter()
        .position(|step| *step == FlattenStep::RenderRoot)
        .unwrap();
    assert!(release > root);
}

const SMALL: SourceId = SourceId::new(601);
const BIG: SourceId = SourceId::new(602);
const BIG_CHILD: SourceId = SourceId::new(603);

/// Root 4x4 draws Small (2x2, 16 bytes) first and Big (8x8, 256 bytes) whose
/// own child is 8x8 too.
fn ordering_fixture() -> Fixture {
    let mut fixture = Fixture::default();
    fixture.add(
        SMALL,
        scene(
            2,
            2,
            TRANSPARENT,
            vec![SourceLayer::new(L1, 0, transform(0, 0, 2, 2))],
        ),
        FlattenMode::Precompose,
    );
    fixture.add(
        BIG_CHILD,
        scene(
            8,
            8,
            Rgba8::new(40, 0, 0, 255),
            vec![premul(L2, 0, transform(1, 1, 6, 6))],
        ),
        FlattenMode::Precompose,
    );
    fixture.add(
        BIG,
        scene(
            8,
            8,
            TRANSPARENT,
            vec![premul(BIG_CHILD, 0, transform(0, 0, 8, 8))],
        ),
        FlattenMode::Precompose,
    );
    fixture.add(
        ROOT,
        scene(
            4,
            4,
            Rgba8::new(0, 0, 90, 255),
            vec![
                SourceLayer::new(L4, 0, transform(0, 0, 4, 4)),
                premul(SMALL, 1, transform(0, 0, 2, 2)),
                premul(BIG, 2, transform(1, 1, 3, 3)).with_opacity(220),
            ],
        ),
        FlattenMode::Auto,
    );
    fixture
}

#[test]
fn larger_working_sets_render_first_and_peak_memory_is_exact() {
    let fixture = ordering_fixture();
    let (plan, output, _) = fixture.assert_equivalent(ROOT, OutputTarget::Program);
    // First-use order (Small, then Big's subtree) would hold 16 + 256 + 256
    // bytes in three slots; rendering Big's subtree first peaks at 512.
    assert_eq!(
        plan.steps(),
        &[
            FlattenStep::Render {
                scene: BIG_CHILD,
                slot: 0
            },
            FlattenStep::Render {
                scene: BIG,
                slot: 1
            },
            FlattenStep::Release {
                scene: BIG_CHILD,
                slot: 0
            },
            FlattenStep::Render {
                scene: SMALL,
                slot: 0
            },
            FlattenStep::RenderRoot,
            FlattenStep::Release {
                scene: SMALL,
                slot: 0
            },
            FlattenStep::Release {
                scene: BIG,
                slot: 1
            },
        ]
    );
    assert_eq!(plan.peak_live_bytes(), 512);
    assert_eq!(plan.peak_live_intermediates(), 2);
    assert_eq!(output.peak_live_bytes, 512);
    assert_eq!(output.renders, vec![BIG_CHILD, BIG, SMALL]);
}

#[test]
fn intermediate_memory_and_slot_limits_are_enforced_at_plan_time() {
    let fixture = ordering_fixture();
    let at_limit = FlattenLimits {
        max_intermediate_bytes: 512,
        max_live_intermediates: 2,
        ..FlattenLimits::default()
    };
    assert!(plan_flatten(&fixture.graph, ROOT, OutputTarget::Program, at_limit).is_ok());
    assert_eq!(
        plan_flatten(
            &fixture.graph,
            ROOT,
            OutputTarget::Program,
            FlattenLimits {
                max_intermediate_bytes: 511,
                ..at_limit
            },
        ),
        Err(FlattenError::IntermediateBytesExceeded {
            required: 512,
            maximum: 511,
        })
    );
    assert_eq!(
        plan_flatten(
            &fixture.graph,
            ROOT,
            OutputTarget::Program,
            FlattenLimits {
                max_live_intermediates: 1,
                ..at_limit
            },
        ),
        Err(FlattenError::LiveIntermediatesExceeded {
            required: 2,
            maximum: 1,
        })
    );
    assert_eq!(
        plan_flatten(
            &fixture.graph,
            ROOT,
            OutputTarget::Program,
            FlattenLimits {
                max_scenes: 3,
                ..at_limit
            },
        ),
        Err(FlattenError::TooManyScenes { maximum: 3 })
    );
}

#[test]
fn deep_nesting_is_exact_bounded_and_uses_two_slots() {
    let chain = (0..=5).map(|level| id(700 + level)).collect::<Vec<_>>();
    let mut fixture = Fixture::default();
    for (level, &scene_id) in chain.iter().enumerate() {
        let shade = u8::try_from(level).unwrap() * 30;
        let mut layers = vec![SourceLayer::new(L1, 0, transform(0, 3, 4, 3))];
        if let Some(&child) = chain.get(level + 1) {
            layers.push(premul(child, 1, transform(1, 1, 5, 5)).with_opacity(240));
        }
        fixture.add(
            scene_id,
            scene(6, 6, Rgba8::new(shade, 0, 60, 255), layers),
            FlattenMode::Precompose,
        );
    }
    let (plan, output, _) = fixture.assert_equivalent(chain[0], OutputTarget::Program);
    assert_eq!(
        output.renders,
        chain[1..].iter().rev().copied().collect::<Vec<_>>()
    );
    assert_eq!(plan.peak_live_intermediates(), 2);
    assert_eq!(plan.peak_live_bytes(), 2 * 6 * 6 * 4);

    let exact = FlattenLimits {
        max_depth: 5,
        ..FlattenLimits::default()
    };
    assert!(plan_flatten(&fixture.graph, chain[0], OutputTarget::Program, exact).is_ok());
    assert_eq!(
        plan_flatten(
            &fixture.graph,
            chain[0],
            OutputTarget::Program,
            FlattenLimits {
                max_depth: 4,
                ..exact
            },
        ),
        Err(FlattenError::DepthExceeded {
            scene: chain[5],
            depth: 5,
            maximum: 4,
        })
    );
}

#[test]
fn depth_counts_the_deepest_path_to_an_already_visited_scene() {
    // Root -> X -> X1 -> X2 and Root -> Y -> Z -> X: X is first reached at
    // depth 1 but its deepest path puts X2 at depth 5.
    let [root, x, y, z, x1, x2] = [750, 751, 752, 753, 754, 755].map(id);
    let mut fixture = Fixture::default();
    let wrap = |child: SourceId| {
        scene(
            4,
            4,
            Rgba8::new(0, 0, 0, 255),
            vec![premul(child, 0, transform(0, 0, 4, 4))],
        )
    };
    fixture.add(
        x2,
        scene(
            4,
            4,
            TRANSPARENT,
            vec![SourceLayer::new(L1, 0, transform(0, 0, 4, 4))],
        ),
        FlattenMode::Auto,
    );
    fixture.add(x1, wrap(x2), FlattenMode::Auto);
    fixture.add(x, wrap(x1), FlattenMode::Auto);
    fixture.add(z, wrap(x), FlattenMode::Auto);
    fixture.add(y, wrap(z), FlattenMode::Auto);
    fixture.add(
        root,
        scene(
            8,
            4,
            TRANSPARENT,
            vec![
                premul(x, 0, transform(0, 0, 4, 4)),
                premul(y, 1, transform(4, 0, 4, 4)),
            ],
        ),
        FlattenMode::Auto,
    );
    let limits = |max_depth| FlattenLimits {
        max_depth,
        ..FlattenLimits::default()
    };
    assert_eq!(
        plan_flatten(&fixture.graph, root, OutputTarget::Program, limits(3)),
        Err(FlattenError::DepthExceeded {
            scene: x,
            depth: 5,
            maximum: 3,
        })
    );
    let plan = plan_flatten(&fixture.graph, root, OutputTarget::Program, limits(5)).unwrap();
    let leaves = leaves();
    let mut oracle_renders = Vec::new();
    let expected = fixture.oracle(root, OutputTarget::Program, &leaves, &mut oracle_renders);
    let output = execute_flatten_cpu(&plan, &bindings(&leaves)).unwrap();
    assert_eq!(output.frame, expected);
}

#[test]
fn scene_used_as_a_key_fill_key_source_is_precomposed() {
    let key = id(900);
    let mut fixture = Fixture::default();
    fixture.add(
        key,
        scene(
            6,
            4,
            Rgba8::new(30, 30, 30, 255),
            vec![premul(L2, 0, transform(1, 0, 4, 4))],
        ),
        FlattenMode::Auto,
    );
    fixture.add(
        ROOT,
        scene(
            10,
            8,
            Rgba8::new(0, 40, 0, 255),
            vec![
                SourceLayer::new(L4, 0, transform(2, 2, 6, 4)).with_key_fill(KeyFill::linear(key)),
            ],
        ),
        FlattenMode::Auto,
    );
    let (plan, output, _) = fixture.assert_equivalent(ROOT, OutputTarget::Program);
    assert_eq!(output.renders, vec![key]);
    let cost = plan.intermediates()[0];
    assert_eq!((cost.consumer_layers, cost.consumer_scenes), (1, 1));
}

#[test]
fn malformed_graphs_and_bindings_fail_with_typed_errors() {
    let mut graph = SceneGraph::new();
    let first = id(800);
    let second = id(801);
    graph
        .insert(
            first,
            scene(
                4,
                4,
                TRANSPARENT,
                vec![premul(second, 0, transform(0, 0, 4, 4))],
            ),
        )
        .unwrap();
    graph
        .insert(
            second,
            scene(
                4,
                4,
                TRANSPARENT,
                vec![premul(first, 0, transform(0, 0, 4, 4))],
            ),
        )
        .unwrap();
    assert_eq!(
        graph.insert(first, scene(2, 2, TRANSPARENT, Vec::new())),
        Err(FlattenError::DuplicateScene(first))
    );
    assert_eq!(
        plan_flatten(
            &graph,
            first,
            OutputTarget::Program,
            FlattenLimits::default()
        ),
        Err(FlattenError::SceneCycle { scene: first })
    );
    assert_eq!(
        plan_flatten(
            &graph,
            id(999),
            OutputTarget::Program,
            FlattenLimits::default()
        ),
        Err(FlattenError::UnknownScene(id(999)))
    );

    let mut broken = SceneGraph::new();
    let nested = id(810);
    broken
        .insert(
            nested,
            scene(
                4,
                4,
                TRANSPARENT,
                vec![
                    SourceLayer::new(L1, 0, transform(0, 0, 4, 4))
                        .with_crop(CropRect::new(0, 0, 0, 2)),
                ],
            ),
        )
        .unwrap();
    broken
        .insert(
            ROOT,
            scene(
                4,
                4,
                TRANSPARENT,
                vec![SourceLayer::new(nested, 0, transform(0, 0, 4, 4))],
            ),
        )
        .unwrap();
    assert_eq!(
        plan_flatten(
            &broken,
            ROOT,
            OutputTarget::Program,
            FlattenLimits::default()
        ),
        Err(FlattenError::Plan {
            scene: nested,
            error: fm_compositor::PlanError::InvalidCrop { layer: 0 },
        })
    );
}

#[test]
fn leaf_bindings_must_not_shadow_scenes_and_must_be_complete() {
    let fixture = ordering_fixture();
    let plan = plan_flatten(
        &fixture.graph,
        ROOT,
        OutputTarget::Program,
        FlattenLimits::default(),
    )
    .unwrap();
    let frames = leaves();
    let mut ambiguous = bindings(&frames);
    ambiguous.push(CpuSourceFrame::new(BIG, &frames[0].1));
    assert!(matches!(
        execute_flatten_cpu(&plan, &ambiguous),
        Err(FlattenExecutionError::AmbiguousSource(source)) if source == BIG
    ));
    let without_l2 = frames
        .iter()
        .filter(|(source, _)| *source != L2)
        .map(|(source, frame)| CpuSourceFrame::new(*source, frame))
        .collect::<Vec<_>>();
    assert!(matches!(
        execute_flatten_cpu(&plan, &without_l2),
        Err(FlattenExecutionError::Scene {
            scene,
            error: fm_compositor::CpuExecutionError::MissingSource(source),
        }) if scene == BIG_CHILD && source == L2
    ));
}

fn timing(sequence: u64) -> MediaTiming {
    MediaTiming::new(
        OriginalTimestamp::new(
            MediaTimestamp::new(i64::try_from(sequence).unwrap()),
            TimeBase::new(1, 1_000).unwrap(),
        ),
        NormalizedTimestamp::from_nanos(i64::try_from(sequence).unwrap() * 1_000_000),
        NormalizedDuration::from_nanos(1_000_000).unwrap(),
        ClockDomainId::new(NonZeroU128::new(11).unwrap()),
        SequenceNumber::new(sequence),
    )
    .unwrap()
}

fn video_frame(frame: &ImageFrame, sequence: u64) -> CpuVideoFrame {
    let payload = CpuVideoPayload::new(
        PixelFormat::Rgba8,
        VideoDimensions::new(frame.width(), frame.height()).unwrap(),
        vec![CpuVideoPlane::new(frame.stride(), frame.pixels().to_vec()).unwrap()],
    )
    .unwrap();
    CpuVideoFrame::new(timing(sequence), payload)
}

#[test]
fn flattening_preserves_caller_output_timing_without_added_delay() {
    let fixture = nested_fixture();
    let leaves = leaves();
    let (plan, output) = fixture.flatten(ROOT, OutputTarget::Program, &leaves);
    let videos = leaves
        .iter()
        .enumerate()
        .map(|(index, (source, frame))| (*source, video_frame(frame, 40 + index as u64)))
        .collect::<Vec<_>>();
    let refs = videos
        .iter()
        .map(|(source, frame)| (*source, frame))
        .collect::<Vec<_>>();
    let output_timing = timing(7);
    let composed = execute_flatten_cpu_frame(&plan, &refs, output_timing).unwrap();
    assert_eq!(composed.timing(), output_timing);
    assert_eq!(
        composed.payload().plane(0).unwrap().bytes(),
        output.frame.pixels()
    );
}

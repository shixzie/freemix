//! IN-024 acceptance: Program and Preview as inputs. A mix feed shows the bus
//! frame committed at the end of the previous tick (black before the first
//! commit), so it can sit on its own bus without a graph cycle; resolution
//! and render planning stay finite and deterministic.

use std::num::NonZeroU128;

use fm_model::{
    FrameContext, Input, InputKind, Layer, LayerGeometry, LayerSource, MainMix, MixFeed,
    MixFeedHistory, MixSelection, Project, ProjectSettings, RenderSource, ResolveError, Rgba8,
    Rotation, Scene, SimulatedAudio, SimulatedInput, SimulatedVideo, SourceRef, SourceResolver,
    ValidationErrorKind,
};
use fm_types::{
    AudioFormat, ChannelLayout, ColorMetadata, FrameRate, InputId, PixelFormat, ProjectId,
    SampleFormat, SampleRate, ScanMode, SceneId, VideoDimensions, VideoFormat,
};

const CAMERA: u128 = 1;
const GRAPHIC: u128 = 2;
const PROGRAM_FEED: u128 = 3;
const PREVIEW_FEED: u128 = 4;
const PROGRAM_ALIAS: u128 = 5;
const MULTIVIEW: u128 = 6;
const NESTED: u128 = 7;

const MULTIVIEW_SCENE: u128 = 1;
const NESTED_SCENE: u128 = 2;

fn input_id(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn scene_id(value: u128) -> SceneId {
    SceneId::new(NonZeroU128::new(value).unwrap())
}

fn settings() -> ProjectSettings {
    let frame_rate = FrameRate::new(30, 1).unwrap();
    ProjectSettings {
        frame_rate,
        video: VideoFormat {
            dimensions: VideoDimensions::new(1920, 1080).unwrap(),
            frame_rate,
            pixel_format: PixelFormat::Rgba8,
            scan: ScanMode::Progressive,
            color: ColorMetadata::default(),
        },
        audio: AudioFormat {
            sample_rate: SampleRate::new(48_000).unwrap(),
            sample_format: SampleFormat::F32,
            channels: ChannelLayout::stereo(),
        },
    }
}

fn input(id: u128, name: &str, kind: InputKind) -> Input {
    Input {
        id: input_id(id),
        name: name.into(),
        kind,
        required_capabilities: Vec::new(),
    }
}

fn simulated() -> InputKind {
    InputKind::Simulated(SimulatedInput::new(
        SimulatedVideo::Bars,
        SimulatedAudio::Silence,
    ))
}

fn layer(source: SourceRef, enabled: bool) -> Layer {
    Layer {
        id: next_layer_id(),
        name: "Cell".into(),
        source,
        enabled,
        geometry: LayerGeometry::new(0, 0, 960, 540, Rotation::Deg0),
        crop: None,
        mask: None,
        key: None,
        effects: Vec::new(),
        opacity: u8::MAX,
        z_order: 0,
    }
}

/// Two leaves, both bus feeds, an alias of the Program feed, a four-cell
/// `MultiView` scene showing camera, Program, Preview, and the alias (plus a
/// disabled cell), and a scene that nests the `MultiView`.
fn feed_project() -> Project {
    let mut project = Project::new(ProjectId::new(NonZeroU128::MIN), "Feeds", settings())
        .with_main_mix(MainMix::new(input_id(MULTIVIEW), input_id(CAMERA)));
    project.add_input(input(CAMERA, "Camera", simulated()));
    project.add_input(input(GRAPHIC, "Graphic", simulated()));
    project.add_input(input(
        PROGRAM_FEED,
        "Program",
        InputKind::MixFeed(MixFeed::Program),
    ));
    project.add_input(input(
        PREVIEW_FEED,
        "Preview",
        InputKind::MixFeed(MixFeed::Preview),
    ));
    project.add_input(input(
        PROGRAM_ALIAS,
        "Program again",
        InputKind::Alias {
            target: input_id(PROGRAM_FEED),
            crop: None,
        },
    ));
    project.add_scene(Scene {
        id: scene_id(MULTIVIEW_SCENE),
        name: "MultiView".into(),
        background: Rgba8::OPAQUE_BLACK,
        layers: vec![
            layer(SourceRef::Input(input_id(CAMERA)), true),
            layer(SourceRef::Input(input_id(PROGRAM_FEED)), true),
            layer(SourceRef::Input(input_id(GRAPHIC)), false),
            layer(SourceRef::Input(input_id(PREVIEW_FEED)), true),
            layer(SourceRef::Input(input_id(PROGRAM_ALIAS)), true),
        ],
        animation: fm_model::SceneAnimation::default(),
    });
    project.add_input(input(
        MULTIVIEW,
        "MultiView",
        InputKind::Scene {
            scene_id: scene_id(MULTIVIEW_SCENE),
            audio_source: None,
        },
    ));
    project.add_scene(Scene {
        id: scene_id(NESTED_SCENE),
        name: "Picture in picture".into(),
        background: Rgba8::OPAQUE_BLACK,
        layers: vec![
            layer(SourceRef::Input(input_id(GRAPHIC)), true),
            layer(SourceRef::Input(input_id(MULTIVIEW)), true),
        ],
        animation: fm_model::SceneAnimation::default(),
    });
    project.add_input(input(
        NESTED,
        "Nested",
        InputKind::Scene {
            scene_id: scene_id(NESTED_SCENE),
            audio_source: None,
        },
    ));
    project
}

fn select(program: u128, preview: u128) -> MixSelection {
    MixSelection {
        program: input_id(program),
        preview: input_id(preview),
    }
}

#[test]
fn feeds_on_their_own_bus_are_valid_because_the_delay_breaks_the_loop() {
    let project = feed_project();
    assert_eq!(project.validate(), Ok(()));

    // Program feed directly on Program and Preview feed directly on Preview.
    let direct =
        feed_project().with_main_mix(MainMix::new(input_id(PROGRAM_FEED), input_id(PREVIEW_FEED)));
    assert_eq!(direct.validate(), Ok(()));

    // A real scene loop is still a cycle even when a feed is also present:
    // the MultiView cell showing the nested scene closes 1 -> 2 -> 1.
    let mut scenes = feed_project().scenes().to_vec();
    scenes[0]
        .layers
        .push(layer(SourceRef::Input(input_id(NESTED)), true));
    let mut looped = Project::new(ProjectId::new(NonZeroU128::MIN), "Loop", settings());
    for input in feed_project().inputs() {
        looped.add_input(input.clone());
    }
    for scene in scenes {
        looped.add_scene(scene);
    }
    let cycles = looped
        .validate()
        .unwrap_err()
        .into_iter()
        .filter(|error| error.kind == ValidationErrorKind::Cycle)
        .count();
    assert_eq!(cycles, 2);
}

#[test]
fn a_feed_is_black_until_the_first_commit_and_then_one_frame_late() {
    let project = feed_project();
    let sources = SourceResolver::new(&project);
    let mut history = MixFeedHistory::<&str>::new();
    {
        let context = sources
            .context(select(PROGRAM_FEED, PREVIEW_FEED), &history)
            .unwrap();
        for (input, feed) in [
            (PROGRAM_FEED, MixFeed::Program),
            (PREVIEW_FEED, MixFeed::Preview),
        ] {
            let resolved = context.resolve_input(input_id(input)).unwrap();
            assert_eq!(resolved.source, RenderSource::Black { feed });
            assert!(resolved.feedback, "{feed:?} is on its own bus");
        }
    }

    history.commit("program 0", "preview 0");
    let context = sources
        .context(select(PROGRAM_FEED, PREVIEW_FEED), &history)
        .unwrap();
    assert_eq!(
        context
            .resolve_input(input_id(PROGRAM_FEED))
            .unwrap()
            .source,
        RenderSource::PreviousFrame {
            feed: MixFeed::Program,
            frame: &"program 0",
        }
    );
    assert_eq!(
        context
            .resolve_input(input_id(PREVIEW_FEED))
            .unwrap()
            .source,
        RenderSource::PreviousFrame {
            feed: MixFeed::Preview,
            frame: &"preview 0",
        }
    );
    // The alias of the Program feed shows the same delayed frame.
    let alias = context.resolve_input(input_id(PROGRAM_ALIAS)).unwrap();
    assert_eq!(alias.alias_chain, [input_id(PROGRAM_ALIAS)]);
    assert_eq!(alias.concrete, input_id(PROGRAM_FEED));
    assert_eq!(
        alias.source,
        RenderSource::PreviousFrame {
            feed: MixFeed::Program,
            frame: &"program 0",
        }
    );

    history.reset();
    let context = sources
        .context(select(PROGRAM_FEED, PREVIEW_FEED), &history)
        .unwrap();
    assert_eq!(
        context
            .resolve_input(input_id(PROGRAM_FEED))
            .unwrap()
            .source,
        RenderSource::Black {
            feed: MixFeed::Program
        }
    );
}

#[test]
fn feedback_is_reported_only_for_a_feed_whose_bus_contains_it() {
    let project = feed_project();
    let sources = SourceResolver::new(&project);
    let history = MixFeedHistory::<u64>::new();

    // Program feed on Preview: a delayed program monitor, not a loop.
    let context = sources
        .context(select(CAMERA, PROGRAM_FEED), &history)
        .unwrap();
    assert!(!context.feedback(MixFeed::Program));
    assert!(!context.feedback(MixFeed::Preview));
    assert!(
        !context
            .resolve_input(input_id(PROGRAM_FEED))
            .unwrap()
            .feedback
    );

    // Program feed on Program: the bus shows its own previous frame.
    let context = sources
        .context(select(PROGRAM_FEED, CAMERA), &history)
        .unwrap();
    assert!(context.feedback(MixFeed::Program));
    assert!(!context.feedback(MixFeed::Preview));

    // The MultiView contains both feeds; on Program it feeds Program back,
    // and nested one level deeper on Preview it feeds Preview back.
    let context = sources
        .context(select(MULTIVIEW, NESTED), &history)
        .unwrap();
    assert!(context.feedback(MixFeed::Program));
    assert!(context.feedback(MixFeed::Preview));

    // A disabled cell does not count: the graphic layer is disabled, and a
    // scene of only leaves never feeds back.
    let context = sources.context(select(GRAPHIC, CAMERA), &history).unwrap();
    assert!(!context.feedback(MixFeed::Program));
    assert!(!context.feedback(MixFeed::Preview));
}

#[test]
fn a_multiview_containing_program_resolves_cells_without_recursing() {
    let project = feed_project();
    let sources = SourceResolver::new(&project);
    let mut history = MixFeedHistory::<u64>::new();
    history.commit(100, 200);
    let context = sources
        .context(select(MULTIVIEW, CAMERA), &history)
        .unwrap();

    let program = context.resolve_input(input_id(MULTIVIEW)).unwrap();
    assert_eq!(
        program.source,
        RenderSource::Scene(scene_id(MULTIVIEW_SCENE))
    );
    // The MultiView is on Program and shows Program, yet planning it needs
    // only itself: the feed is a terminal, not an expansion of Program.
    assert_eq!(
        context.scenes_in_render_order(input_id(MULTIVIEW)),
        Ok(vec![scene_id(MULTIVIEW_SCENE)])
    );
    assert_eq!(
        context.scenes_in_render_order(input_id(NESTED)),
        Ok(vec![scene_id(MULTIVIEW_SCENE), scene_id(NESTED_SCENE)])
    );
    assert_eq!(
        context.scenes_in_render_order(input_id(PROGRAM_FEED)),
        Ok(vec![])
    );

    let scene = context.resolve_scene(scene_id(MULTIVIEW_SCENE)).unwrap();
    let cells = scene
        .layers
        .iter()
        .map(|layer| {
            let LayerSource::Input(resolved) = &layer.source else {
                panic!("the MultiView has only input cells");
            };
            (layer.index, resolved.source, resolved.feedback)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        cells,
        [
            (0, RenderSource::Input(input_id(CAMERA)), false),
            (
                1,
                RenderSource::PreviousFrame {
                    feed: MixFeed::Program,
                    frame: &100,
                },
                true
            ),
            (
                3,
                RenderSource::PreviousFrame {
                    feed: MixFeed::Preview,
                    frame: &200,
                },
                false
            ),
            (
                4,
                RenderSource::PreviousFrame {
                    feed: MixFeed::Program,
                    frame: &100,
                },
                true
            ),
        ]
    );

    let nested = context.resolve_scene(scene_id(NESTED_SCENE)).unwrap();
    assert!(matches!(
        &nested.layers[1].source,
        LayerSource::Input(resolved)
            if resolved.source == RenderSource::Scene(scene_id(MULTIVIEW_SCENE))
    ));
    assert_eq!(
        context.resolve_scene(scene_id(9)),
        Err(ResolveError::UnknownScene(scene_id(9)))
    );
}

/// A deterministic stand-in renderer: a leaf frame depends on its id and the
/// tick, a scene folds its enabled cells in order, and a feed contributes the
/// committed frame (zero for black).
fn render(context: &FrameContext<'_, '_, '_, u64>, input: InputId, tick: u64) -> u64 {
    let resolved = context.resolve_input(input).unwrap();
    match resolved.source {
        RenderSource::Input(leaf) => u64::try_from(leaf.get().get()).unwrap() * 1_000 + tick,
        RenderSource::Black { .. } => 0,
        RenderSource::PreviousFrame { frame, .. } => *frame,
        RenderSource::Scene(scene) => context.resolve_scene(scene).unwrap().layers.iter().fold(
            17,
            |accumulator: u64, layer| {
                let cell = match &layer.source {
                    LayerSource::Input(cell) => render(context, cell.requested, tick),
                    LayerSource::Scene(_) => unreachable!("fixture has no direct scene layers"),
                };
                accumulator.wrapping_mul(1_000_003).wrapping_add(cell)
            },
        ),
    }
}

fn run(project: &Project, selections: &[MixSelection]) -> Vec<(u64, u64)> {
    let sources = SourceResolver::new(project);
    let mut history = MixFeedHistory::new();
    let mut frames = Vec::new();
    for (tick, selection) in (0_u64..).zip(selections) {
        let (program, preview) = {
            let context = sources.context(*selection, &history).unwrap();
            (
                render(&context, selection.program, tick),
                render(&context, selection.preview, tick),
            )
        };
        // Commit happens after every source of this tick was rendered; the
        // borrow checker forbids committing while `context` is alive.
        history.commit(program, preview);
        frames.push((program, preview));
    }
    frames
}

#[test]
fn feedback_over_ticks_is_a_deterministic_one_frame_recurrence() {
    let project = feed_project();
    let selections = [
        select(MULTIVIEW, CAMERA),
        select(MULTIVIEW, CAMERA),
        select(MULTIVIEW, PROGRAM_FEED),
        select(PROGRAM_FEED, MULTIVIEW),
        select(NESTED, PREVIEW_FEED),
        select(PREVIEW_FEED, NESTED),
    ];
    let first = run(&project, &selections);
    let second = run(&project, &selections);
    assert_eq!(first, second);

    // Tick 0: every feed is black. The MultiView folds camera, black Program,
    // black Preview, and black Program (via the alias).
    let fold = |cells: [u64; 4]| {
        cells.into_iter().fold(17_u64, |accumulator, cell| {
            accumulator.wrapping_mul(1_000_003).wrapping_add(cell)
        })
    };
    assert_eq!(first[0], (fold([1_000, 0, 0, 0]), 1_000));
    // Tick 1 sees exactly tick 0's committed frames.
    assert_eq!(
        first[1],
        (fold([1_001, first[0].0, first[0].1, first[0].0]), 1_001)
    );
    // Program feed on Preview shows the previous Program frame.
    assert_eq!(first[2].1, first[1].0);
    // Program feed on Program freezes the previous Program frame.
    assert_eq!(first[3].0, first[2].0);
    // Preview feed on Preview repeats the previous Preview frame.
    assert_eq!(first[4].1, first[3].1);
    assert_eq!(first[5].0, first[4].1);
}

/// A process-unique layer id, so layers built by helpers never collide.
fn next_layer_id() -> fm_model::LayerId {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);
    fm_model::LayerId::new(NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
        .expect("layer ids start at one")
}

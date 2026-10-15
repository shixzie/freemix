//! IN-025 acceptance: virtual inputs (aliases) reuse a source under their own
//! identity, name, list position, crop, and audio strip; validation rejects
//! missing targets and reference cycles; the resolver maps every alias chain
//! to its concrete source.

use std::num::NonZeroU128;

use fm_model::{
    AudioRoute, CropRect, EntityRef, Input, InputAudioStripState, InputGainMilliDb, InputKind,
    Layer, LayerGeometry, MainMix, MixFeed, MixFeedHistory, MixSelection, Project, ProjectSettings,
    RemoveInputError, RenderSource, ReplaceInputError, ResolveError, Rgba8, Rotation, Scene,
    SimulatedAudio, SimulatedInput, SimulatedVideo, SourceRef, SourceResolver, ValidationError,
    ValidationErrorKind,
};
use fm_types::{
    AudioFormat, ChannelLayout, ColorMetadata, FrameRate, InputId, PixelFormat, ProjectId,
    SampleFormat, SampleRate, ScanMode, SceneId, VideoDimensions, VideoFormat,
};

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

fn bars() -> InputKind {
    InputKind::Simulated(SimulatedInput::new(
        SimulatedVideo::Bars,
        SimulatedAudio::Sine { frequency_hz: 440 },
    ))
}

fn alias(target: u128, crop: Option<CropRect>) -> InputKind {
    InputKind::Alias {
        target: input_id(target),
        crop,
    }
}

fn layer(source: SourceRef) -> Layer {
    Layer {
        id: next_layer_id(),
        name: "Layer".into(),
        source,
        enabled: true,
        geometry: LayerGeometry::new(0, 0, 1920, 1080, Rotation::Deg0),
        crop: None,
        mask: None,
        key: None,
        effects: Vec::new(),
        opacity: u8::MAX,
        z_order: 0,
    }
}

/// Camera 1, a cropped alias 2 of it, and an uncropped alias 3 of alias 2.
fn aliased_project() -> Project {
    let mut project = Project::new(ProjectId::new(NonZeroU128::MIN), "Show", settings());
    project.add_input(input(1, "Camera", bars()));
    project.add_input(input(
        2,
        "Camera close",
        alias(1, Some(CropRect::new(480, 270, 960, 540))),
    ));
    project.add_input(input(3, "Camera again", alias(2, None)));
    project
}

fn errors(project: &Project) -> Vec<ValidationError> {
    project.validate().err().unwrap_or_default()
}

fn has_error(project: &Project, entity: EntityRef, field: &str, kind: ValidationErrorKind) -> bool {
    errors(project)
        .iter()
        .any(|error| error.entity == Some(entity) && error.field == field && error.kind == kind)
}

#[test]
fn an_alias_has_its_own_identity_name_position_and_audio_strip() {
    let mut project = aliased_project();
    assert_eq!(project.validate(), Ok(()));

    let names = project
        .inputs()
        .iter()
        .map(|input| (input.id, input.name.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        [
            (input_id(1), "Camera"),
            (input_id(2), "Camera close"),
            (input_id(3), "Camera again"),
        ]
    );

    // Renaming and reordering the alias leaves the target untouched.
    project
        .rename_input(input_id(2), "Close-up".into())
        .unwrap();
    project
        .reorder_inputs(&[input_id(2), input_id(3), input_id(1)])
        .unwrap();
    assert_eq!(project.inputs()[0].name, "Close-up");
    assert_eq!(project.inputs()[2].name, "Camera");

    // Each alias owns an audio strip that changes independently.
    let quiet = InputAudioStripState {
        gain: InputGainMilliDb::new(-12_000).unwrap(),
        muted: true,
        ..InputAudioStripState::default()
    };
    assert!(project.set_input_audio_strip(input_id(2), quiet));
    assert_eq!(project.input_audio_strip(input_id(2)), Some(quiet));
    assert_eq!(
        project.input_audio_strip(input_id(1)),
        Some(InputAudioStripState::default())
    );
    assert_eq!(
        project.input_audio_strip(input_id(3)),
        Some(InputAudioStripState::default())
    );
    assert_eq!(project.validate(), Ok(()));
}

#[test]
fn alias_chains_resolve_to_the_concrete_source_with_only_the_requested_crop() {
    let project = aliased_project();
    let resolver = SourceResolver::new(&project);
    let history = MixFeedHistory::<u64>::new();
    let context = resolver
        .context(
            MixSelection {
                program: input_id(3),
                preview: input_id(1),
            },
            &history,
        )
        .unwrap();

    let direct = context.resolve_input(input_id(1)).unwrap();
    assert_eq!(direct.alias_chain, []);
    assert_eq!(direct.concrete, input_id(1));
    assert_eq!(direct.crop, None);
    assert_eq!(direct.source, RenderSource::Input(input_id(1)));

    let close = context.resolve_input(input_id(2)).unwrap();
    assert_eq!(close.alias_chain, [input_id(2)]);
    assert_eq!(close.concrete, input_id(1));
    assert_eq!(close.crop, Some(CropRect::new(480, 270, 960, 540)));
    assert_eq!(close.source, RenderSource::Input(input_id(1)));

    // Alias 3 targets alias 2 but does not inherit its crop.
    let again = context.resolve_input(input_id(3)).unwrap();
    assert_eq!(again.alias_chain, [input_id(3), input_id(2)]);
    assert_eq!(again.concrete, input_id(1));
    assert_eq!(again.crop, None);
    assert_eq!(again.source, RenderSource::Input(input_id(1)));
    assert!(!again.feedback);

    // Audio follows the chain to the concrete source; the strip applied is
    // still the requested alias's own.
    assert_eq!(
        resolver.resolve_audio(input_id(3)),
        Ok(AudioRoute::Input(input_id(1)))
    );
}

#[test]
fn an_alias_and_its_target_may_sit_on_program_and_preview_together() {
    let project = aliased_project().with_main_mix(MainMix::new(input_id(1), input_id(2)));
    assert_eq!(project.validate(), Ok(()));
}

#[test]
fn validation_rejects_missing_self_and_cyclic_alias_targets() {
    let mut missing = aliased_project();
    missing.add_input(input(4, "Dangling", alias(99, None)));
    assert!(has_error(
        &missing,
        EntityRef::Input(input_id(4)),
        "kind.alias.target",
        ValidationErrorKind::MissingReference(EntityRef::Input(input_id(99)))
    ));

    let mut selfish = aliased_project();
    selfish.add_input(input(4, "Mirror", alias(4, None)));
    assert!(has_error(
        &selfish,
        EntityRef::Input(input_id(4)),
        "kind.alias.target",
        ValidationErrorKind::SelfReference
    ));
    assert!(has_error(
        &selfish,
        EntityRef::Input(input_id(4)),
        "kind.alias.target",
        ValidationErrorKind::Cycle
    ));

    // 4 -> 5 -> 6 -> 4, with alias 7 leading into the cycle.
    let mut cyclic = aliased_project();
    cyclic.add_input(input(4, "A", alias(5, None)));
    cyclic.add_input(input(5, "B", alias(6, None)));
    cyclic.add_input(input(6, "C", alias(4, None)));
    cyclic.add_input(input(7, "Lead-in", alias(4, None)));
    let cycle_members = errors(&cyclic)
        .into_iter()
        .filter(|error| error.kind == ValidationErrorKind::Cycle)
        .map(|error| (error.entity, error.field))
        .collect::<Vec<_>>();
    assert_eq!(
        cycle_members,
        [4, 5, 6].map(|id| (Some(EntityRef::Input(input_id(id))), "kind.alias.target"))
    );

    // The resolver refuses the same graphs rather than looping.
    let resolver = SourceResolver::new(&cyclic);
    let history = MixFeedHistory::<u64>::new();
    let context = resolver
        .context(
            MixSelection {
                program: input_id(1),
                preview: input_id(2),
            },
            &history,
        )
        .unwrap();
    assert_eq!(
        context.resolve_input(input_id(7)),
        Err(ResolveError::AliasCycle(input_id(7)))
    );
    assert_eq!(
        resolver.resolve_audio(input_id(5)),
        Err(ResolveError::AliasCycle(input_id(5)))
    );
    let resolver = SourceResolver::new(&missing);
    let context = resolver
        .context(
            MixSelection {
                program: input_id(1),
                preview: input_id(2),
            },
            &history,
        )
        .unwrap();
    assert_eq!(
        context.resolve_input(input_id(4)),
        Err(ResolveError::UnknownInput(input_id(99)))
    );
    assert!(matches!(
        resolver.context(
            MixSelection {
                program: input_id(4),
                preview: input_id(2),
            },
            &history,
        ),
        Err(ResolveError::UnknownInput(id)) if id == input_id(99)
    ));
}

#[test]
fn alias_crops_must_be_nonempty_and_inside_the_project_frame() {
    for crop in [
        CropRect::new(0, 0, 0, 10),
        CropRect::new(0, 0, 10, 0),
        CropRect::new(1, 0, 1920, 1080),
        CropRect::new(0, u32::MAX, 10, 10),
    ] {
        let mut project = aliased_project();
        project.add_input(input(4, "Bad crop", alias(1, Some(crop))));
        assert!(
            has_error(
                &project,
                EntityRef::Input(input_id(4)),
                "kind.alias.crop",
                ValidationErrorKind::OutOfRange
            ),
            "{crop:?}"
        );
    }
}

#[test]
fn an_alias_closing_a_scene_loop_is_a_scene_cycle() {
    // Scene 1 is shown by scene input 10; alias 11 re-presents input 10, and
    // scene 1 shows alias 11, so the scene would contain itself.
    let mut project = aliased_project();
    project.add_scene(Scene {
        id: scene_id(1),
        name: "Loop".into(),
        background: Rgba8::OPAQUE_BLACK,
        layers: vec![
            layer(SourceRef::Input(input_id(1))),
            layer(SourceRef::Input(input_id(11))),
        ],
        animation: fm_model::SceneAnimation::default(),
    });
    project.add_input(input(
        10,
        "Loop scene",
        InputKind::Scene {
            scene_id: scene_id(1),
            audio_source: None,
        },
    ));
    project.add_input(input(11, "Loop alias", alias(10, None)));
    assert!(has_error(
        &project,
        EntityRef::Scene(scene_id(1)),
        "layers.source",
        ValidationErrorKind::Cycle
    ));

    let resolver = SourceResolver::new(&project);
    let history = MixFeedHistory::<u64>::new();
    let context = resolver
        .context(
            MixSelection {
                program: input_id(1),
                preview: input_id(2),
            },
            &history,
        )
        .unwrap();
    assert_eq!(
        context.scenes_in_render_order(input_id(11)),
        Err(ResolveError::SceneCycle(scene_id(1)))
    );
}

#[test]
fn an_alias_closing_an_audio_loop_is_an_audio_cycle_and_mix_feeds_are_silent() {
    let mut project = aliased_project();
    project.add_scene(Scene {
        id: scene_id(1),
        name: "Scene".into(),
        background: Rgba8::OPAQUE_BLACK,
        layers: vec![layer(SourceRef::Input(input_id(1)))],
        animation: fm_model::SceneAnimation::default(),
    });
    // Scene input 10 takes its audio from alias 11, which re-presents 10.
    project.add_input(input(
        10,
        "Scene",
        InputKind::Scene {
            scene_id: scene_id(1),
            audio_source: Some(input_id(11)),
        },
    ));
    project.add_input(input(11, "Scene alias", alias(10, None)));
    let audio_cycles = errors(&project)
        .into_iter()
        .filter(|error| error.kind == ValidationErrorKind::Cycle)
        .collect::<Vec<_>>();
    assert_eq!(
        audio_cycles,
        [ValidationError {
            entity: Some(EntityRef::Input(input_id(10))),
            field: "kind.scene.audio_source",
            kind: ValidationErrorKind::Cycle,
        }]
    );
    assert_eq!(
        SourceResolver::new(&project).resolve_audio(input_id(11)),
        Err(ResolveError::AliasCycle(input_id(11)))
    );

    // Pointed at the camera alias instead, the scene hears the camera.
    project
        .set_scene_input_audio_source(input_id(10), Some(input_id(3)))
        .unwrap();
    assert_eq!(project.validate(), Ok(()));
    let resolver = SourceResolver::new(&project);
    assert_eq!(
        resolver.resolve_audio(input_id(11)),
        Ok(AudioRoute::Input(input_id(1)))
    );

    // A bus feed, and any alias of it, carries no audio.
    project.add_input(input(
        12,
        "Program feed",
        InputKind::MixFeed(MixFeed::Program),
    ));
    project.add_input(input(13, "Program feed alias", alias(12, None)));
    let resolver = SourceResolver::new(&project);
    assert_eq!(
        resolver.resolve_audio(input_id(13)),
        Ok(AudioRoute::Silence)
    );
}

#[test]
fn removing_or_retargeting_respects_alias_references() {
    let mut project = aliased_project();
    let before = project.clone();
    assert_eq!(
        project.remove_input(input_id(1)),
        Err(RemoveInputError::DomainReference(input_id(1)))
    );
    assert_eq!(
        project.remove_input(input_id(2)),
        Err(RemoveInputError::DomainReference(input_id(2)))
    );
    assert_eq!(
        project.replace_input_source(input_id(3), alias(42, None), Vec::new()),
        Err(ReplaceInputError::MissingInput(input_id(42)))
    );
    // Pointing the camera at alias 3 would close 1 -> 3 -> 2 -> 1.
    assert_eq!(
        project.replace_input_source(input_id(1), alias(3, None), Vec::new()),
        Err(ReplaceInputError::SourceCycle)
    );
    assert_eq!(project, before);

    // Leaves can be removed once nothing aliases them.
    project.remove_input(input_id(3)).unwrap();
    project.remove_input(input_id(2)).unwrap();
    project.remove_input(input_id(1)).unwrap();
    assert_eq!(project.inputs(), []);
    assert_eq!(project.input_audio_strips(), []);
}

#[test]
fn long_alias_chains_and_cycles_are_handled_iteratively() {
    const LENGTH: u128 = 4_000;
    let mut chain = Project::new(ProjectId::new(NonZeroU128::MIN), "Chain", settings());
    chain.add_input(input(1, "Leaf", bars()));
    for id in 2..=LENGTH {
        chain.add_input(input(id, &format!("Alias {id}"), alias(id - 1, None)));
    }
    assert_eq!(chain.validate(), Ok(()));
    let sources = SourceResolver::new(&chain);
    let history = MixFeedHistory::<u64>::new();
    let context = sources
        .context(
            MixSelection {
                program: input_id(LENGTH),
                preview: input_id(1),
            },
            &history,
        )
        .unwrap();
    let resolved = context.resolve_input(input_id(LENGTH)).unwrap();
    assert_eq!(resolved.concrete, input_id(1));
    assert_eq!(
        resolved.alias_chain.len(),
        usize::try_from(LENGTH - 1).unwrap()
    );

    // Closing the chain makes every alias a cycle member, and the leaf-less
    // ring is reported once per member without recursion.
    chain
        .replace_input_source(input_id(1), alias(LENGTH, None), Vec::new())
        .unwrap_err();
    let mut inputs = chain.inputs().to_vec();
    inputs[0].kind = alias(LENGTH, None);
    let mut ring = Project::new(ProjectId::new(NonZeroU128::MIN), "Ring", settings());
    for input in inputs {
        ring.add_input(input);
    }
    let cycles = errors(&ring)
        .into_iter()
        .filter(|error| error.kind == ValidationErrorKind::Cycle)
        .count();
    assert_eq!(cycles, usize::try_from(LENGTH).unwrap());
}

/// A process-unique layer id, so layers built by helpers never collide.
fn next_layer_id() -> fm_model::LayerId {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);
    fm_model::LayerId::new(NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
        .expect("layer ids start at one")
}

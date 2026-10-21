use std::num::NonZeroU128;

use fm_model::{
    AddInputError, AddSceneInputError, AddSceneLayerError, AddStreamTargetError, AudioBus, BusSend,
    CURRENT_SCHEMA_VERSION, ChromaLayerKey, CropRect, DEFAULT_VIDEO_BITRATE_KBPS,
    DuplicateSceneInputError, EntityRef, Input, InputAudioStripState, InputBalanceBasisPoints,
    InputDelaySamples, InputGainMilliDb, InputKind, Layer, LayerAnchor, LayerEffect,
    LayerEffectError, LayerGeometry, LayerKey, LayerKeyError, LumaLayerKey,
    MAX_STREAM_ENDPOINT_BYTES, MAX_STREAM_LADDER_HEIGHT, MAX_STREAM_LADDER_RUNGS,
    MAX_STREAM_LADDER_WIDTH, MAX_VIDEO_BITRATE_KBPS, MIN_STREAM_LADDER_DIMENSION,
    MIN_STREAM_LADDER_RUNGS, MIN_VIDEO_BITRATE_KBPS, MainMix, Output, OutputFormat, Project,
    ProjectSettings, RECORDER_SLOT_COUNT, REDACTED_STREAM_KEY, RecorderSlot, RecorderSlotError,
    RectMask, RemoveAudioBusError, RemoveInputError, RemoveOutputError, RemoveSceneError,
    RenameSceneError, ReplaceInputError, RestartPolicy, Rgba8, Rotation, Scene, SceneLayerError,
    SetInputLockError, SetStingerError, SimulatedAudio, SimulatedInput, SimulatedVideo, SolidColor,
    SourceRef, StartupPolicy, StingerAudioPolicy, StingerConfig, StingerMissingMediaFallback,
    StingerSlotNumber, StreamEndpoint, StreamEndpointError, StreamKey, StreamKeyError,
    StreamLadder, StreamLadderError, StreamLadderRung, StreamProtocol, StreamTarget,
    StreamTargetError, StreamTargetId, StreamVideoBitrateError, ValidationError,
    ValidationErrorKind,
};
use fm_types::{
    AudioFormat, BusId, ChannelLayout, ColorMetadata, FrameRate, InputId, MAX_INPUT_NAME_BYTES,
    OutputId, PixelFormat, ProjectId, SampleFormat, SampleRate, ScanMode, SceneId, VideoDimensions,
    VideoFormat,
};

fn project_id(value: u128) -> ProjectId {
    ProjectId::new(NonZeroU128::new(value).unwrap())
}

fn input_id(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn scene_id(value: u128) -> SceneId {
    SceneId::new(NonZeroU128::new(value).unwrap())
}

fn bus_id(value: u128) -> BusId {
    BusId::new(NonZeroU128::new(value).unwrap())
}

fn output_id(value: u128) -> OutputId {
    OutputId::new(NonZeroU128::new(value).unwrap())
}

fn settings() -> ProjectSettings {
    let frame_rate = FrameRate::new(60_000, 1_001).unwrap();
    ProjectSettings {
        frame_rate,
        video: VideoFormat {
            dimensions: VideoDimensions::new(1920, 1080).unwrap(),
            frame_rate,
            pixel_format: PixelFormat::Nv12,
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

fn identity_geometry() -> LayerGeometry {
    LayerGeometry::new(0, 0, 1920, 1080, Rotation::Deg0)
}

fn layer(name: &str, source: SourceRef) -> Layer {
    Layer {
        id: next_layer_id(),
        name: name.into(),
        source,
        enabled: true,
        geometry: identity_geometry(),
        crop: None,
        mask: None,
        key: None,
        effects: Vec::new(),
        opacity: u8::MAX,
        z_order: 0,
    }
}

fn valid_project() -> Project {
    let mut project = Project::new(project_id(1), "Main show", settings());
    project.add_input(Input {
        id: input_id(1),
        name: "Camera".into(),
        kind: InputKind::Device {
            stable_key: "camera-1".into(),
            capture: fm_types::capture::CaptureSettings::default(),
        },
        required_capabilities: vec!["capture.camera.raw".into()],
    });
    project.add_scene(Scene {
        id: scene_id(1),
        name: "Wide".into(),
        background: Rgba8::OPAQUE_BLACK,
        layers: vec![layer("Camera", SourceRef::Input(input_id(1)))],
        animation: fm_model::SceneAnimation::default(),
    });
    project.add_audio_bus(AudioBus {
        id: bus_id(1),
        name: "Master".into(),
        sends: Vec::new(),
    });
    project.add_output(Output {
        id: output_id(1),
        name: "Program".into(),
        video_source: scene_id(1),
        audio_source: bus_id(1),
        startup: StartupPolicy::Stopped,
        required_capabilities: vec!["gpu.compositor.wgpu".into()],
    });
    project
}

#[test]
fn input_source_replacement_refuses_missing_scenes_and_scene_cycles() {
    let mut project = valid_project();
    let before = project.clone();

    assert_eq!(
        project.replace_input_source(
            input_id(1),
            InputKind::Scene {
                scene_id: scene_id(9),
                audio_source: None,
            },
            Vec::new(),
        ),
        Err(ReplaceInputError::MissingScene(scene_id(9)))
    );
    assert_eq!(project, before);

    // Scene 1's layer references input 1, so pointing input 1 back at scene 1
    // would make the graph reach itself.
    assert_eq!(
        project.replace_input_source(
            input_id(1),
            InputKind::Scene {
                scene_id: scene_id(1),
                audio_source: None,
            },
            Vec::new(),
        ),
        Err(ReplaceInputError::SourceCycle)
    );
    assert_eq!(project, before);

    project
        .replace_input_source(
            input_id(1),
            InputKind::Media {
                asset_uri: "asset://clip.mp4".into(),
                playback: fm_model::MediaPlaybackSettings::default(),
            },
            Vec::new(),
        )
        .unwrap();
    assert_ne!(project, before);
}

#[test]
fn scene_layer_crop_checked_validation_is_atomic_and_mask_aware() {
    let scene = scene_id(1);
    let mut project = valid_project();
    project
        .set_scene_layer_crop(scene, 0, Some(CropRect::new(0, 0, 1920, 1080)))
        .unwrap();
    project
        .set_scene_layer_mask(scene, 0, Some(RectMask::new(10, 20, 100, 100)))
        .unwrap();
    let before = project.clone();

    for (crop, error) in [
        (
            CropRect::new(0, 0, 0, 10),
            SceneLayerError::InvalidCrop { scene, index: 0 },
        ),
        (
            CropRect::new(1900, 0, 21, 10),
            SceneLayerError::InvalidCrop { scene, index: 0 },
        ),
        (
            CropRect::new(u32::MAX, 0, 1, 10),
            SceneLayerError::InvalidCrop { scene, index: 0 },
        ),
        (
            CropRect::new(0, 0, 20, 20),
            SceneLayerError::CropWouldInvalidateMask { scene, index: 0 },
        ),
    ] {
        assert_eq!(
            project.set_scene_layer_crop(scene, 0, Some(crop)),
            Err(error)
        );
        assert_eq!(project, before);
        assert_eq!(project.scenes()[0].layers, before.scenes()[0].layers);
    }

    project
        .set_scene_layer_crop(scene, 0, Some(CropRect::new(100, 100, 640, 480)))
        .unwrap();
    assert_eq!(
        project.scenes()[0].layers[0].crop,
        Some(CropRect::new(100, 100, 640, 480))
    );
    project.set_scene_layer_crop(scene, 0, None).unwrap();
    assert_eq!(project.scenes()[0].layers[0].crop, None);
}

#[test]
fn scene_layer_mask_checked_validation_is_atomic_and_crop_aware() {
    let scene = scene_id(1);
    let mut project = valid_project();
    let before = project.clone();
    for (mask, error) in [
        (
            RectMask::new(0, 0, 0, 1),
            SceneLayerError::InvalidMask { scene, index: 0 },
        ),
        (
            RectMask::new(1_900, 0, 21, 1),
            SceneLayerError::InvalidMask { scene, index: 0 },
        ),
        (
            RectMask::new(u32::MAX, 0, 1, 1),
            SceneLayerError::InvalidMask { scene, index: 0 },
        ),
    ] {
        assert_eq!(
            project.set_scene_layer_mask(scene, 0, Some(mask)),
            Err(error)
        );
        assert_eq!(project, before);
    }
    project
        .set_scene_layer_crop(scene, 0, Some(CropRect::new(100, 100, 640, 480)))
        .unwrap();
    let before_crop_failure = project.clone();
    assert_eq!(
        project.set_scene_layer_mask(scene, 0, Some(RectMask::new(639, 0, 2, 1))),
        Err(SceneLayerError::InvalidMask { scene, index: 0 })
    );
    assert_eq!(project, before_crop_failure);
    project
        .set_scene_layer_mask(scene, 0, Some(RectMask::new(0, 0, 640, 480)))
        .unwrap();
    project.set_scene_layer_mask(scene, 0, None).unwrap();
    assert_eq!(project.scenes()[0].layers[0].mask, None);
}

#[test]
fn scene_layer_geometry_checked_validation_is_atomic() {
    let scene = scene_id(1);
    let mut project = valid_project();
    let before = project.clone();
    for (width, height) in [
        (0, 100),
        (100, 0),
        (ProjectSettings::MAX_VIDEO_WIDTH + 1, 100),
        (100, ProjectSettings::MAX_VIDEO_HEIGHT + 1),
    ] {
        let geometry = LayerGeometry::new(1, 2, width, height, Rotation::Deg90);
        assert_eq!(
            project.set_scene_layer_geometry(scene, 0, geometry),
            Err(SceneLayerError::InvalidGeometry { scene, index: 0 })
        );
        assert_eq!(project, before);
    }
    // A stored tilt stays below half a quarter turn either way, and the
    // anchor within the box.
    for geometry in [
        LayerGeometry::new(1, 2, 100, 100, Rotation::Deg0).with_tilt(45_000),
        LayerGeometry::new(1, 2, 100, 100, Rotation::Deg0).with_tilt(-45_001),
        LayerGeometry::new(1, 2, 100, 100, Rotation::Deg0)
            .with_anchor(LayerAnchor::new(LayerAnchor::MAX + 1, 0)),
        LayerGeometry::new(1, 2, 100, 100, Rotation::Deg0)
            .with_anchor(LayerAnchor::new(0, LayerAnchor::MAX + 1)),
    ] {
        assert_eq!(
            project.set_scene_layer_geometry(scene, 0, geometry),
            Err(SceneLayerError::InvalidGeometry { scene, index: 0 })
        );
        assert_eq!(project, before);
    }

    let geometry = LayerGeometry::new(-12, 34, 640, 480, Rotation::Deg270)
        .with_tilt(-45_000)
        .with_anchor(LayerAnchor::new(0, LayerAnchor::MAX));
    project
        .set_scene_layer_geometry(scene, 0, geometry)
        .unwrap();
    assert_eq!(project.scenes()[0].layers[0].geometry, geometry);
}

fn simulated_project() -> Project {
    let output_format = OutputFormat {
        video: settings().video,
        audio: settings().audio,
    };
    let mut project = Project::new(
        project_id(2),
        "Simulated show",
        ProjectSettings::new(FrameRate::new(60_000, 1_001).unwrap(), output_format),
    )
    .with_main_mix(MainMix::new(input_id(1), input_id(2)))
    .with_restart_policy(RestartPolicy::OnFailure { max_attempts: 3 });
    project.add_input(Input {
        id: input_id(1),
        name: "Red".into(),
        kind: InputKind::Simulated(SimulatedInput::new(
            SimulatedVideo::Solid(SolidColor::new(255, 0, 0, 255)),
            SimulatedAudio::Silence,
        )),
        required_capabilities: Vec::new(),
    });
    project.add_input(Input {
        id: input_id(2),
        name: "Bars and tone".into(),
        kind: InputKind::Simulated(SimulatedInput::new(
            SimulatedVideo::Bars,
            SimulatedAudio::Sine {
                frequency_hz: 1_000,
            },
        )),
        required_capabilities: Vec::new(),
    });
    project
}

#[test]
fn coherent_project_is_valid() {
    assert_eq!(valid_project().validate(), Ok(()));
}

#[test]
fn add_scene_layer_rejects_missing_sources_and_cycles_without_mutation() {
    let mut project = valid_project();
    let before = project.clone();
    assert_eq!(
        project.add_layer_to_scene(scene_id(1), layer(" \t", SourceRef::Input(input_id(99)))),
        Err(AddSceneLayerError::EmptyName)
    );
    assert_eq!(project, before);

    for (source, error) in [
        (
            SourceRef::Input(input_id(99)),
            AddSceneLayerError::MissingInput(input_id(99)),
        ),
        (
            SourceRef::Scene(scene_id(99)),
            AddSceneLayerError::MissingScene(scene_id(99)),
        ),
        (
            SourceRef::Scene(scene_id(1)),
            AddSceneLayerError::SourceCycle,
        ),
    ] {
        let before = project.clone();
        assert_eq!(
            project.add_layer_to_scene(scene_id(1), layer("rejected", source)),
            Err(error)
        );
        assert_eq!(project, before);
    }

    project.add_scene(Scene {
        id: scene_id(2),
        name: "Nested".into(),
        background: Rgba8::OPAQUE_BLACK,
        layers: vec![layer("back", SourceRef::Scene(scene_id(1)))],
        animation: fm_model::SceneAnimation::default(),
    });
    project.add_input(Input {
        id: input_id(2),
        name: "Nested input".into(),
        kind: InputKind::Scene {
            scene_id: scene_id(2),
            audio_source: None,
        },
        required_capabilities: Vec::new(),
    });
    let before = project.clone();
    assert_eq!(
        project.add_layer_to_scene(scene_id(1), layer("cross", SourceRef::Input(input_id(2)))),
        Err(AddSceneLayerError::SourceCycle)
    );
    assert_eq!(project, before);

    let appended = layer("accepted", SourceRef::Input(input_id(1)));
    project
        .add_layer_to_scene(scene_id(1), appended.clone())
        .unwrap();
    assert_eq!(project.scenes()[0].layers.last(), Some(&appended));
}

#[test]
fn rename_scene_preserves_exact_text_and_rejects_invalid_names_without_mutation() {
    let mut project = Project::new(project_id(80), "Rename", settings());
    project.add_scene(Scene {
        id: scene_id(1),
        name: "Wide".into(),
        background: Rgba8::OPAQUE_BLACK,
        layers: Vec::new(),
        animation: fm_model::SceneAnimation::default(),
    });
    project.add_scene(Scene {
        id: scene_id(2),
        name: "Close".into(),
        background: Rgba8::OPAQUE_BLACK,
        layers: Vec::new(),
        animation: fm_model::SceneAnimation::default(),
    });
    project
        .rename_scene(scene_id(1), "  Exact scene  ".into())
        .unwrap();
    assert_eq!(project.scenes()[0].name, "  Exact scene  ");

    for (scene, name, error) in [
        (
            scene_id(99),
            "Unused",
            RenameSceneError::UnknownScene(scene_id(99)),
        ),
        (scene_id(1), " \t ", RenameSceneError::EmptyName),
        (scene_id(1), "close", RenameSceneError::DuplicateName),
    ] {
        let before = project.clone();
        assert_eq!(project.rename_scene(scene, name.into()), Err(error));
        assert_eq!(project, before);
    }
}

#[test]
fn duplicate_scene_input_is_atomic_and_preserves_layer_fields() {
    use DuplicateSceneInputError::*;

    let mut project = Project::new(project_id(81), "Duplicate", settings());
    project.add_input(Input {
        id: input_id(1),
        name: "Camera".into(),
        kind: InputKind::Color,
        required_capabilities: vec!["capture.camera".into()],
    });
    let mut source_layer = layer("Inset", SourceRef::Input(input_id(1)));
    source_layer.enabled = false;
    source_layer.geometry = LayerGeometry::new(10, 20, 640, 360, Rotation::Deg90);
    source_layer.crop = Some(CropRect::new(100, 50, 300, 200));
    source_layer.mask = Some(RectMask::new(20, 30, 100, 80).inverted(true));
    source_layer.opacity = 200;
    source_layer.z_order = -3;
    project.add_scene(Scene {
        id: scene_id(1),
        name: "Source".into(),
        background: Rgba8::new(8, 4, 2, 16),
        layers: vec![source_layer],
        animation: fm_model::SceneAnimation::default(),
    });
    project
        .duplicate_scene_input_checked(
            scene_id(1),
            scene_id(2),
            "Shared".into(),
            input_id(2),
            "Shared".into(),
        )
        .unwrap();
    let mut expected_scene = project.scenes()[0].clone();
    expected_scene.id = scene_id(2);
    expected_scene.name = "Shared".into();
    assert_eq!(
        project.scenes(),
        &[project.scenes()[0].clone(), expected_scene]
    );
    assert_eq!(project.inputs()[1].id, input_id(2));
    assert_eq!(project.inputs()[1].name, "Shared");
    assert_eq!(
        project.inputs()[1].kind,
        InputKind::Scene {
            scene_id: scene_id(2),
            audio_source: None,
        }
    );
    assert_eq!(
        project.inputs()[1].required_capabilities,
        [] as [std::string::String; 0]
    );
    assert_eq!(
        project.input_audio_strip(input_id(2)),
        Some(InputAudioStripState::default())
    );

    let mut reject = |source: SceneId,
                      new_scene: SceneId,
                      new_input: InputId,
                      scene_name: &str,
                      input_name: &str,
                      error: DuplicateSceneInputError| {
        let before = project.clone();
        assert_eq!(
            project.duplicate_scene_input_checked(
                source,
                new_scene,
                scene_name.to_owned(),
                new_input,
                input_name.to_owned(),
            ),
            Err(error),
        );
        assert_eq!(project, before);
    };
    let (s1, s3, s99) = (scene_id(1), scene_id(3), scene_id(99));
    let (i1, i3) = (input_id(1), input_id(3));
    reject(s99, s3, i3, "New", "New", UnknownSourceScene(s99));
    reject(s1, s1, i3, "New", "New", DuplicateSceneId(s1));
    reject(s1, s3, i1, "New", "New", DuplicateInputId(i1));
    reject(s1, s3, i3, " ", "New", EmptySceneName);
    reject(s1, s3, i3, "source", "New", DuplicateSceneName);
    reject(s1, s3, i3, "New", "\t", EmptyInputName);
    reject(
        s1,
        s3,
        i3,
        "New",
        &"x".repeat(MAX_INPUT_NAME_BYTES + 1),
        InputNameTooLong,
    );
    reject(s1, s3, i3, "New", "Camera", DuplicateInputName);
}

#[test]
fn add_scene_input_is_atomic_and_uses_current_name_contract() {
    use AddSceneInputError::*;

    let mut project = valid_project();
    project
        .add_scene_input_checked(scene_id(2), "  Exact  ".into(), input_id(2), "Exact".into())
        .unwrap();
    assert_eq!(project.scenes()[1].name, "  Exact  ");
    assert_eq!(project.inputs()[1].name, "Exact");
    assert_eq!(project.scenes()[1].background, Rgba8::OPAQUE_BLACK);
    assert!(matches!(
        project.inputs()[1].kind,
        InputKind::Scene {
            scene_id: added_scene,
            audio_source: None
        } if added_scene == scene_id(2)
    ));
    assert_eq!(
        project.inputs()[1].required_capabilities,
        [] as [std::string::String; 0]
    );
    assert_eq!(
        project.input_audio_strip(input_id(2)),
        Some(InputAudioStripState::default())
    );

    let mut reject = |scene: SceneId, input: InputId, scene_name: &str, input_name: &str, error| {
        let before = project.clone();
        assert_eq!(
            project.add_scene_input_checked(
                scene,
                scene_name.to_owned(),
                input,
                input_name.to_owned(),
            ),
            Err(error)
        );
        assert_eq!(project, before);
    };
    for (scene, input, scene_name, input_name, error) in [
        (
            scene_id(2),
            input_id(3),
            "New",
            "New",
            DuplicateSceneId(scene_id(2)),
        ),
        (
            scene_id(3),
            input_id(1),
            "New",
            "New",
            DuplicateInputId(input_id(1)),
        ),
        (scene_id(3), input_id(3), " ", "New", EmptySceneName),
        (scene_id(3), input_id(3), "wide", "New", DuplicateSceneName),
        (scene_id(3), input_id(3), "New", "\t", EmptyInputName),
        (
            scene_id(3),
            input_id(3),
            "New",
            "Camera",
            DuplicateInputName,
        ),
    ] {
        reject(scene, input, scene_name, input_name, error);
    }
    reject(
        scene_id(3),
        input_id(3),
        "New",
        &"x".repeat(MAX_INPUT_NAME_BYTES + 1),
        InputNameTooLong,
    );

    let mut invalid = project.clone();
    invalid.add_input(Input {
        id: input_id(9),
        name: "x".repeat(MAX_INPUT_NAME_BYTES + 1),
        kind: InputKind::Color,
        required_capabilities: Vec::new(),
    });
    let expected = invalid.validate().unwrap_err();
    let before = invalid.clone();
    assert_eq!(
        invalid.add_scene_input_checked(scene_id(9), "New".into(), input_id(10), "New".into()),
        Err(InvalidProject(expected))
    );
    assert_eq!(invalid, before);
}

#[test]
fn complete_simulated_production_is_valid() {
    let project = simulated_project();

    assert_eq!(project.validate(), Ok(()));
    assert_eq!(
        project.main_mix(),
        Some(MainMix::new(input_id(1), input_id(2)))
    );
    assert_eq!(
        project.restart_policy(),
        RestartPolicy::OnFailure { max_attempts: 3 }
    );
    assert_eq!(
        project.settings().output_format().video.frame_rate,
        project.settings().frame_rate
    );
}

#[test]
fn remove_input_removes_pair_and_rejects_domain_references() {
    type Mutation = Box<dyn FnOnce(&mut Project)>;
    let mut base = Project::new(project_id(70), "Remove", settings());
    for id in [1, 2, 3] {
        base.add_input(Input {
            id: input_id(id),
            name: format!("Input {id}"),
            kind: InputKind::Simulated(SimulatedInput::new(
                SimulatedVideo::Bars,
                SimulatedAudio::Silence,
            )),
            required_capabilities: Vec::new(),
        });
    }
    let mut unused = base.clone();
    unused.remove_input(input_id(3)).unwrap();
    assert!(!unused.inputs().iter().any(|input| input.id == input_id(3)));
    assert!(
        !unused
            .input_audio_strips()
            .iter()
            .any(|strip| strip.input == input_id(3))
    );

    let mut cases: Vec<Mutation> = vec![
        Box::new(|project| project.set_main_mix(MainMix::new(input_id(3), input_id(1)))),
        Box::new(|project| {
            project.add_stinger(StingerConfig::new(
                StingerSlotNumber::new(1).unwrap(),
                input_id(3),
                false,
                0,
                StingerAudioPolicy::Muted,
                StingerMissingMediaFallback::Cut,
            ));
        }),
        Box::new(|project| {
            project.add_scene(Scene {
                id: scene_id(3),
                name: "Scene".into(),
                background: Rgba8::OPAQUE_BLACK,
                layers: vec![layer("Input", SourceRef::Input(input_id(3)))],
                animation: fm_model::SceneAnimation::default(),
            });
        }),
        Box::new(|project| {
            project.add_input(Input {
                id: input_id(4),
                name: "Scene input".into(),
                kind: InputKind::Scene {
                    scene_id: scene_id(3),
                    audio_source: Some(input_id(3)),
                },
                required_capabilities: Vec::new(),
            });
        }),
    ];
    for configure in cases.drain(..) {
        let mut project = base.clone();
        configure(&mut project);
        assert_eq!(
            project.remove_input(input_id(3)),
            Err(RemoveInputError::DomainReference(input_id(3)))
        );
    }
}

#[test]
fn add_input_checked_is_atomic_and_preserves_exact_name() {
    let mut project = valid_project();
    let candidate = |id: u128, name: &str| Input {
        id: input_id(id),
        name: name.into(),
        kind: InputKind::Simulated(SimulatedInput::new(
            SimulatedVideo::Bars,
            SimulatedAudio::Silence,
        )),
        required_capabilities: Vec::new(),
    };
    project
        .add_input_checked(candidate(2, "Exact  name  "))
        .unwrap();
    assert_eq!(project.inputs()[1].name, "Exact  name  ");
    assert_eq!(
        project.input_audio_strip(input_id(2)),
        Some(InputAudioStripState::default())
    );
    assert_eq!(
        project
            .inputs()
            .iter()
            .map(|input| input.id)
            .collect::<Vec<_>>(),
        vec![input_id(1), input_id(2)]
    );
    project.add_input_checked(candidate(3, "camera")).unwrap();
    for (input, error) in [
        (
            candidate(1, "Other"),
            AddInputError::DuplicateId(input_id(1)),
        ),
        (candidate(4, "   "), AddInputError::EmptyName),
        (
            candidate(4, &"x".repeat(MAX_INPUT_NAME_BYTES + 1)),
            AddInputError::NameTooLong,
        ),
        (candidate(4, "Exact  name  "), AddInputError::DuplicateName),
    ] {
        let before = project.clone();
        assert_eq!(project.add_input_checked(input), Err(error));
        assert_eq!(project, before);
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn remove_outputs_and_audio_buses_preserves_order_and_rejects_references() {
    let mut base = valid_project();
    base.add_audio_bus(AudioBus {
        id: bus_id(2),
        name: "Aux".into(),
        sends: Vec::new(),
    });
    base.add_output(Output {
        id: output_id(2),
        name: "Auxiliary".into(),
        video_source: scene_id(1),
        audio_source: bus_id(2),
        startup: StartupPolicy::Stopped,
        required_capabilities: Vec::new(),
    });

    let mut unknown_output = base.clone();
    let snapshot = unknown_output.clone();
    assert_eq!(
        unknown_output.remove_output(output_id(99)),
        Err(RemoveOutputError::UnknownOutput(output_id(99)))
    );
    assert_eq!(unknown_output, snapshot);

    let mut output = base.clone();
    output.remove_output(output_id(1)).unwrap();
    assert_eq!(
        output
            .outputs()
            .iter()
            .map(|item| item.id)
            .collect::<Vec<_>>(),
        vec![output_id(2)]
    );

    for (bus, expected) in [
        (bus_id(99), RemoveAudioBusError::UnknownBus(bus_id(99))),
        (
            bus_id(1),
            RemoveAudioBusError::OutputReference {
                bus: bus_id(1),
                output: output_id(1),
            },
        ),
    ] {
        let mut project = base.clone();
        let snapshot = project.clone();
        assert_eq!(project.remove_audio_bus(bus), Err(expected));
        assert_eq!(project, snapshot);
    }

    let mut outgoing = valid_project();
    outgoing.remove_output(output_id(1)).unwrap();
    outgoing.add_audio_bus(AudioBus {
        id: bus_id(2),
        name: "Aux".into(),
        sends: vec![BusSend {
            destination: bus_id(3),
        }],
    });
    outgoing.add_audio_bus(AudioBus {
        id: bus_id(3),
        name: "Record".into(),
        sends: Vec::new(),
    });
    let snapshot = outgoing.clone();
    assert_eq!(
        outgoing.remove_audio_bus(bus_id(2)),
        Err(RemoveAudioBusError::OutgoingSend {
            bus: bus_id(2),
            destination: bus_id(3),
        })
    );
    assert_eq!(outgoing, snapshot);

    let mut incoming = valid_project();
    incoming.remove_output(output_id(1)).unwrap();
    incoming.add_audio_bus(AudioBus {
        id: bus_id(2),
        name: "Aux".into(),
        sends: vec![BusSend {
            destination: bus_id(3),
        }],
    });
    incoming.add_audio_bus(AudioBus {
        id: bus_id(3),
        name: "Record".into(),
        sends: Vec::new(),
    });
    let snapshot = incoming.clone();
    assert_eq!(
        incoming.remove_audio_bus(bus_id(3)),
        Err(RemoveAudioBusError::IncomingSend {
            bus: bus_id(3),
            source: bus_id(2),
        })
    );
    assert_eq!(incoming, snapshot);

    let mut ordered = valid_project();
    ordered.remove_output(output_id(1)).unwrap();
    ordered.add_audio_bus(AudioBus {
        id: bus_id(2),
        name: "Aux".into(),
        sends: Vec::new(),
    });
    ordered.add_audio_bus(AudioBus {
        id: bus_id(3),
        name: "Record".into(),
        sends: Vec::new(),
    });
    ordered.remove_audio_bus(bus_id(2)).unwrap();
    assert_eq!(
        ordered
            .audio_buses()
            .iter()
            .map(|item| item.id)
            .collect::<Vec<_>>(),
        vec![bus_id(1), bus_id(3)]
    );
}

#[test]
fn bad_routing_generator_rate_and_format_are_reported() {
    let frame_rate = FrameRate::new(241, 1).unwrap();
    let mismatched_video_rate = FrameRate::new(60, 1).unwrap();
    let mut project = Project::new(
        project_id(3),
        "Broken simulation",
        ProjectSettings {
            frame_rate,
            video: VideoFormat {
                dimensions: VideoDimensions::new(8_193, 1080).unwrap(),
                frame_rate: mismatched_video_rate,
                pixel_format: PixelFormat::Rgba8,
                scan: ScanMode::Progressive,
                color: ColorMetadata::default(),
            },
            audio: AudioFormat {
                sample_rate: SampleRate::new(1_000).unwrap(),
                sample_format: SampleFormat::F32,
                channels: ChannelLayout::stereo(),
            },
        },
    )
    .with_main_mix(MainMix::new(input_id(1), input_id(99)))
    .with_restart_policy(RestartPolicy::OnFailure { max_attempts: 0 });
    project.add_input(Input {
        id: input_id(1),
        name: "Bad tone".into(),
        kind: InputKind::Simulated(SimulatedInput::new(
            SimulatedVideo::Bars,
            SimulatedAudio::Sine { frequency_hz: 0 },
        )),
        required_capabilities: Vec::new(),
    });

    let errors = project.validate().unwrap_err();
    for field in [
        "settings.frame_rate",
        "settings.video.frame_rate",
        "settings.video.dimensions",
        "settings.audio.sample_rate",
        "restart_policy.max_attempts",
        "kind.simulated.audio.frequency_hz",
        "main_mix.desired_preview",
    ] {
        assert!(
            errors.iter().any(|error| error.field == field),
            "missing validation error for {field}"
        );
    }
}

fn canvas_project(width: u32, height: u32, pixel_format: PixelFormat) -> Project {
    let mut settings = settings();
    settings.video.dimensions = VideoDimensions::new(width, height).unwrap();
    settings.video.pixel_format = pixel_format;
    let mut project = Project::new(project_id(4), "Vertical show", settings);
    project.add_input(Input {
        id: input_id(1),
        name: "Camera".into(),
        kind: InputKind::Device {
            stable_key: "camera-1".into(),
            capture: fm_types::capture::CaptureSettings::default(),
        },
        required_capabilities: Vec::new(),
    });
    let mut full = layer("Full canvas", SourceRef::Input(input_id(1)));
    full.geometry = LayerGeometry::new(0, 0, width, height, Rotation::Deg0);
    // A quarter-turned layer authored in pre-rotation size: its footprint is
    // the whole canvas with the axes swapped.
    let mut turned = layer("Turned", SourceRef::Input(input_id(1)));
    turned.geometry = LayerGeometry::new(0, 0, height, width, Rotation::Deg90);
    turned.z_order = 1;
    project.add_scene(Scene {
        id: scene_id(1),
        name: "Vertical".into(),
        background: Rgba8::OPAQUE_BLACK,
        layers: vec![full, turned],
        animation: fm_model::SceneAnimation::default(),
    });
    project
}

#[test]
fn portrait_square_and_odd_canvases_validate_within_the_format_bounds() {
    for (width, height, format) in [
        (1_080, 1_920, PixelFormat::Nv12),
        (1_080, 1_080, PixelFormat::Nv12),
        (2_160, 3_840, PixelFormat::P010),
        (1_080, 1_921, PixelFormat::Yuv422),
        (1_081, 1_921, PixelFormat::Rgba8),
        (ProjectSettings::MAX_VIDEO_WIDTH, 1, PixelFormat::Bgra8),
        (
            1,
            ProjectSettings::MAX_VIDEO_HEIGHT,
            PixelFormat::Rgba16Float,
        ),
    ] {
        assert_eq!(
            canvas_project(width, height, format).validate(),
            Ok(()),
            "{width}x{height} {format:?}"
        );
    }
    for (width, height, format) in [
        (1_081, 1_920, PixelFormat::Nv12),
        (1_080, 1_921, PixelFormat::Nv12),
        (1_079, 1_079, PixelFormat::P010),
        (1_081, 1_920, PixelFormat::Yuv422),
    ] {
        assert_eq!(
            canvas_project(width, height, format).validate(),
            Err(vec![ValidationError {
                entity: None,
                field: "settings.video.dimensions",
                kind: ValidationErrorKind::FormatMismatch,
            }]),
            "{width}x{height} {format:?}"
        );
    }
}

#[test]
fn duplicate_routes_are_rejected() {
    let mut project = simulated_project();
    project.set_main_mix(MainMix::new(input_id(1), input_id(1)));
    project.add_audio_bus(AudioBus {
        id: bus_id(1),
        name: "Master".into(),
        sends: Vec::new(),
    });
    project.add_audio_bus(AudioBus {
        id: bus_id(2),
        name: "Aux".into(),
        sends: vec![
            BusSend {
                destination: bus_id(1),
            },
            BusSend {
                destination: bus_id(1),
            },
        ],
    });

    let errors = project.validate().unwrap_err();
    assert_eq!(
        errors
            .iter()
            .filter(|error| error.kind == ValidationErrorKind::DuplicateReference)
            .count(),
        2
    );
}

#[test]
fn project_uses_the_current_schema_contract() {
    assert_eq!(valid_project().schema_version(), CURRENT_SCHEMA_VERSION);
}

fn stream_target_id(value: u128) -> StreamTargetId {
    StreamTargetId::new(NonZeroU128::new(value).unwrap())
}

fn stream_target(id: u128, name: &str, output: OutputId) -> StreamTarget {
    StreamTarget::new(
        stream_target_id(id),
        name.to_owned(),
        StreamProtocol::Rtmps,
        StreamEndpoint::parse("ingest.example:443/live").unwrap(),
        Some(StreamKey::parse("sk-abcdef-012345").unwrap()),
        output,
    )
    .unwrap()
}

fn ladder_rung(video_bitrate_kbps: u32, width: u32, height: u32) -> StreamLadderRung {
    StreamLadderRung::new(video_bitrate_kbps, width, height).unwrap()
}

#[test]
fn stream_destinations_refuse_bad_urls_credentials_duplicate_ids_and_dangling_outputs() {
    for (url, expected) in [
        (
            "http://ingest.example/live",
            StreamEndpointError::UnsupportedScheme,
        ),
        (
            "ingest.example/live",
            StreamEndpointError::UnsupportedScheme,
        ),
        (
            "rtmps://operator:hunter2@ingest.example/live",
            StreamEndpointError::EmbeddedCredentials,
        ),
        ("rtmps:///live", StreamEndpointError::MissingHost),
        (
            "rtmps://ingest.example",
            StreamEndpointError::MissingApplicationPath,
        ),
        (
            "rtmps://ingest.example/live/",
            StreamEndpointError::EmptyPathSegment,
        ),
        (
            "rtmps://ingest.example/live?token=abcdef",
            StreamEndpointError::QueryOrFragment,
        ),
        (
            "rtmps://ingest.example/li ve",
            StreamEndpointError::InvalidCharacter,
        ),
    ] {
        assert_eq!(
            StreamEndpoint::parse_url(url).map(|(_, endpoint)| endpoint),
            Err(expected),
            "{url} must be refused"
        );
    }
    assert_eq!(StreamKey::parse("abc"), Err(StreamKeyError::TooShort));
    assert_eq!(
        StreamKey::parse("has/slash"),
        Err(StreamKeyError::PathSeparator)
    );

    let mut project = valid_project();
    project
        .add_stream_target_checked(stream_target(1, "Primary", output_id(1)))
        .unwrap();
    assert!(project.validate().is_ok());

    assert_eq!(
        project.add_stream_target_checked(stream_target(1, "Backup", output_id(1))),
        Err(AddStreamTargetError::DuplicateId(stream_target_id(1)))
    );
    assert_eq!(
        project.add_stream_target_checked(stream_target(2, "pRiMaRy", output_id(1))),
        Err(AddStreamTargetError::DuplicateName)
    );
    assert_eq!(
        project.add_stream_target_checked(stream_target(2, "Backup", output_id(9))),
        Err(AddStreamTargetError::UnknownOutput(output_id(9)))
    );
    assert_eq!(project.stream_targets().len(), 1);

    // A sixth destination is refused (OR-006): the engine, the switcher and
    // the output set hold five, so a sixth saved offline made the bundle
    // unloadable.
    let mut full = project.clone();
    for id in 2..=5 {
        full.add_stream_target_checked(stream_target(id, &format!("Extra {id}"), output_id(1)))
            .unwrap();
    }
    assert_eq!(full.stream_targets().len(), fm_model::MAX_STREAM_TARGETS);
    assert_eq!(
        full.add_stream_target_checked(stream_target(6, "Sixth", output_id(1))),
        Err(AddStreamTargetError::TooMany)
    );
    assert_eq!(full.stream_targets().len(), fm_model::MAX_STREAM_TARGETS);

    // An output that a destination publishes cannot be pulled out from under it.
    assert_eq!(
        project.remove_output(output_id(1)),
        Err(RemoveOutputError::StreamTargetReference {
            output: output_id(1),
            stream_target: stream_target_id(1),
        })
    );

    // The unchecked path is what the decoder uses, so whole-project
    // validation has to catch a dangling output on its own.
    let mut dangling = valid_project();
    dangling.add_stream_target(stream_target(1, "Primary", output_id(9)));
    let errors = dangling.validate().unwrap_err();
    assert!(errors.contains(&ValidationError {
        entity: Some(EntityRef::StreamTarget(stream_target_id(1))),
        field: "output",
        kind: ValidationErrorKind::MissingReference(EntityRef::Output(output_id(9))),
    }));
}

#[test]
fn a_stream_destination_a_cue_names_cannot_be_removed() {
    // A destination a cue starts or stops cannot be removed either; the
    // refusal names the cue (RC-017).
    let mut cued = valid_project();
    cued.add_stream_target_checked(stream_target(1, "Primary", output_id(1)))
        .unwrap();
    cued.set_cue_lists(vec![fm_model::CueList {
        id: 7,
        name: "Show".into(),
        cues: vec![fm_model::Cue {
            id: 10,
            name: "Go live".into(),
            actions: vec![fm_model::CueAction::Stream {
                target: stream_target_id(1),
                start: true,
            }],
            auto_follow: false,
        }],
        state: fm_model::CueRunState {
            standby: Some(10),
            ..fm_model::CueRunState::default()
        },
    }]);
    assert!(cued.validate().is_ok());
    let refused = cued.remove_stream_target(stream_target_id(1));
    assert_eq!(
        refused,
        Err(fm_model::RemoveStreamTargetError::CueReference {
            target: stream_target_id(1),
            list: 7,
            cue: 10,
        })
    );
    assert_eq!(
        refused.unwrap_err().to_string(),
        "stream destination 1 is in use by cue 10 of cue list 7"
    );
    assert_eq!(cued.stream_targets().len(), 1);
}

#[test]
fn srt_endpoints_take_host_port_only_and_recompose_through_a_streamid_query() {
    const KEY: &str = "srt-live-9f3c-key";

    // The port is optional and the endpoint is stored scheme-free.
    assert_eq!(
        StreamEndpoint::parse_url("srt://ingest.example.test:9710").unwrap(),
        (
            StreamProtocol::Srt,
            StreamEndpoint::parse_for(StreamProtocol::Srt, "ingest.example.test:9710").unwrap()
        )
    );
    assert_eq!(
        StreamEndpoint::parse_url("srt://ingest.example.test")
            .unwrap()
            .1
            .as_str(),
        "ingest.example.test"
    );

    for (endpoint, expected) in [
        (
            "ingest.example.test:9710/live",
            StreamEndpointError::PathNotAllowed,
        ),
        ("ingest.example.test/", StreamEndpointError::PathNotAllowed),
        (
            "ingest.example.test?streamid=abcdef",
            StreamEndpointError::QueryOrFragment,
        ),
        (
            "ingest.example.test#fragment",
            StreamEndpointError::QueryOrFragment,
        ),
        (
            "operator:hunter2@ingest.example.test:9710",
            StreamEndpointError::EmbeddedCredentials,
        ),
        (":9710", StreamEndpointError::MissingHost),
        ("", StreamEndpointError::Empty),
        (
            "ingest.example.test:9710 li",
            StreamEndpointError::InvalidCharacter,
        ),
    ] {
        assert_eq!(
            StreamEndpoint::parse_for(StreamProtocol::Srt, endpoint),
            Err(expected),
            "{endpoint} must be refused"
        );
    }

    // A full URL dispatches on its scheme: paths stay legal for RTMP but not
    // for SRT, and unknown schemes are still refused outright.
    assert_eq!(
        StreamEndpoint::parse_url("srt://ingest.example.test/live"),
        Err(StreamEndpointError::PathNotAllowed)
    );
    assert_eq!(
        StreamEndpoint::parse_url("rtmps://ingest.example.test/live"),
        Ok((
            StreamProtocol::Rtmps,
            StreamEndpoint::parse("ingest.example.test/live").unwrap()
        ))
    );
    assert_eq!(
        StreamEndpoint::parse_url("udp://ingest.example.test:9710"),
        Err(StreamEndpointError::UnsupportedScheme)
    );

    // Recomposition puts the key into the stream id query, and redaction
    // scrubs it there too.
    let target = StreamTarget::new(
        stream_target_id(7),
        "SRT relay".to_owned(),
        StreamProtocol::Srt,
        StreamEndpoint::parse_url("srt://ingest.example.test:9710")
            .unwrap()
            .1,
        Some(StreamKey::parse(KEY).unwrap()),
        output_id(1),
    )
    .unwrap()
    .with_backup_endpoint(Some(
        StreamEndpoint::parse_url("srt://backup.example.test")
            .unwrap()
            .1,
    ))
    .unwrap();
    assert_eq!(
        target.expose_url(),
        format!("srt://ingest.example.test:9710?streamid={KEY}")
    );
    assert_eq!(
        target.expose_backup_url(),
        Some(format!("srt://backup.example.test?streamid={KEY}"))
    );
    assert_eq!(
        target.redacted_url(),
        "srt://ingest.example.test:9710?streamid=****"
    );
    assert_eq!(
        target.redacted_backup_url().as_deref(),
        Some("srt://backup.example.test?streamid=****")
    );

    // The strict RTMP-style entry point still demands an application path,
    // so a bare host:port stays an authoring error there.
    assert_eq!(
        StreamEndpoint::parse("ingest.example.test:9710"),
        Err(StreamEndpointError::MissingApplicationPath)
    );
}

#[test]
fn hls_endpoints_take_absolute_directory_paths_and_reject_escaping_ones() {
    // A full URL stores exactly what follows `hls://`, so the leading slash
    // belongs to the path and recomposition restores the full URL.
    assert_eq!(
        StreamEndpoint::parse_url("hls:///var/lib/freemix/live").unwrap(),
        (
            StreamProtocol::Hls,
            StreamEndpoint::parse_for(StreamProtocol::Hls, "/var/lib/freemix/live").unwrap()
        )
    );
    assert_eq!(StreamProtocol::Hls.scheme(), "hls");
    assert_eq!(
        StreamEndpoint::parse_url("hls:///var/lib/freemix/live")
            .unwrap()
            .1
            .as_str(),
        "/var/lib/freemix/live"
    );

    for (endpoint, expected) in [
        ("var/lib/freemix/live", StreamEndpointError::PathNotAbsolute),
        ("./live", StreamEndpointError::PathNotAbsolute),
        ("", StreamEndpointError::Empty),
        ("/", StreamEndpointError::EmptyPathSegment),
        (
            "/var/lib/freemix/live/",
            StreamEndpointError::EmptyPathSegment,
        ),
        ("/var//freemix/live", StreamEndpointError::EmptyPathSegment),
        ("/var/lib/../live", StreamEndpointError::PathTraversal),
        ("/var/lib/./live", StreamEndpointError::PathTraversal),
        ("/var/lib/..", StreamEndpointError::PathTraversal),
        (
            "/var/lib/freemix/live?token=abcdef",
            StreamEndpointError::QueryOrFragment,
        ),
        (
            "/var/lib/freemix/live#segment",
            StreamEndpointError::QueryOrFragment,
        ),
        (
            "hls:///var/lib/freemix/live",
            StreamEndpointError::EmbeddedScheme,
        ),
        ("/var/lib/free mix", StreamEndpointError::InvalidCharacter),
    ] {
        assert_eq!(
            StreamEndpoint::parse_for(StreamProtocol::Hls, endpoint),
            Err(expected),
            "{endpoint} must be refused"
        );
    }

    // The shared guards run before the path rules, so the bound still holds.
    assert_eq!(
        StreamEndpoint::parse_for(
            StreamProtocol::Hls,
            &format!("/{}", "a".repeat(MAX_STREAM_ENDPOINT_BYTES))
        ),
        Err(StreamEndpointError::TooLong)
    );

    // An absolute directory stays invalid for RTMP, whose authority would be
    // empty, so the HLS shape does not leak into the other protocols.
    assert_eq!(
        StreamEndpoint::parse("/var/lib/freemix/live"),
        Err(StreamEndpointError::MissingHost)
    );
}

#[test]
fn hls_destinations_author_a_local_directory_without_a_key_or_backup() {
    let endpoint = StreamEndpoint::parse_url("hls:///var/lib/freemix/live")
        .unwrap()
        .1;
    let target = StreamTarget::new(
        stream_target_id(3),
        "Local HLS".to_owned(),
        StreamProtocol::Hls,
        endpoint.clone(),
        None,
        output_id(1),
    )
    .unwrap();

    assert_eq!(target.key(), None);
    assert_eq!(target.endpoint(), &endpoint);
    assert_eq!(target.expose_url(), "hls:///var/lib/freemix/live");
    assert_eq!(target.redacted_url(), "hls:///var/lib/freemix/live");
    assert_eq!(target.expose_backup_url(), None);
    assert_eq!(target.redacted_backup_url(), None);

    // An HLS destination has no key to redact, so no surface renders the
    // placeholder or leaves the directory.
    for rendered in [
        format!("{target:?}"),
        format!("{target}"),
        target.redacted_url(),
    ] {
        assert!(rendered.contains("hls:///var/lib/freemix/live"));
        assert!(
            !rendered.contains(REDACTED_STREAM_KEY),
            "unexpected redaction in `{rendered}`"
        );
    }

    // HLS refuses a key...
    assert_eq!(
        StreamTarget::new(
            stream_target_id(3),
            "Keyed HLS".to_owned(),
            StreamProtocol::Hls,
            endpoint.clone(),
            Some(StreamKey::parse("sk-abcdef-012345").unwrap()),
            output_id(1),
        ),
        Err(StreamTargetError::UnexpectedStreamKey)
    );

    // ...and a failover endpoint, which has no local equivalent. Clearing
    // one stays allowed and changes nothing.
    assert_eq!(
        target.clone().with_backup_endpoint(Some(endpoint)),
        Err(StreamTargetError::BackupNotAllowed)
    );
    assert_eq!(target.clone().with_backup_endpoint(None).unwrap(), target);
}

#[test]
fn stream_targets_refuse_endpoints_validated_for_another_protocol() {
    let key = || Some(StreamKey::parse("sk-abcdef-012345").unwrap());
    let rtmp = StreamEndpoint::parse("ingest.example/live").unwrap();
    let srt = StreamEndpoint::parse_for(StreamProtocol::Srt, "ingest.example:9710").unwrap();
    let hls = StreamEndpoint::parse_for(StreamProtocol::Hls, "/var/lib/freemix/live").unwrap();
    for (protocol, endpoint, key) in [
        // SRT names only host[:port]; an RTMP application path is refused.
        (StreamProtocol::Srt, rtmp.clone(), key()),
        // An HLS directory has no host, so RTMP would compose `rtmp:///...`.
        (StreamProtocol::Rtmp, hls.clone(), key()),
        // A bare SRT host has no application path to hold the RTMP key.
        (StreamProtocol::Rtmps, srt.clone(), key()),
        // HLS writes to an absolute local directory, not a remote host.
        (StreamProtocol::Hls, rtmp.clone(), None),
    ] {
        assert_eq!(
            StreamTarget::new(
                stream_target_id(5),
                "Mismatched".to_owned(),
                protocol,
                endpoint,
                key,
                output_id(1),
            ),
            Err(StreamTargetError::EndpointProtocolMismatch),
            "{protocol} must refuse an endpoint validated for another protocol"
        );
    }

    let target = StreamTarget::new(
        stream_target_id(5),
        "SRT".to_owned(),
        StreamProtocol::Srt,
        srt,
        key(),
        output_id(1),
    )
    .unwrap();
    assert_eq!(
        target.clone().with_backup_endpoint(Some(rtmp)),
        Err(StreamTargetError::EndpointProtocolMismatch)
    );
    assert_eq!(
        target.with_backup_endpoint(Some(hls)),
        Err(StreamTargetError::EndpointProtocolMismatch)
    );
}

#[test]
fn remote_protocols_require_a_stream_key() {
    for (protocol, endpoint) in [
        (
            StreamProtocol::Rtmp,
            StreamEndpoint::parse("ingest.example/live").unwrap(),
        ),
        (
            StreamProtocol::Rtmps,
            StreamEndpoint::parse("ingest.example:443/live").unwrap(),
        ),
        (
            StreamProtocol::Srt,
            StreamEndpoint::parse_for(StreamProtocol::Srt, "ingest.example:9710").unwrap(),
        ),
    ] {
        assert_eq!(
            StreamTarget::new(
                stream_target_id(5),
                "No key".to_owned(),
                protocol,
                endpoint.clone(),
                None,
                output_id(1),
            ),
            Err(StreamTargetError::MissingStreamKey),
            "{protocol} must require a stream key"
        );

        let target = StreamTarget::new(
            stream_target_id(5),
            "Keyed".to_owned(),
            protocol,
            endpoint,
            Some(StreamKey::parse("sk-abcdef-012345").unwrap()),
            output_id(1),
        )
        .unwrap();
        assert!(target.key().is_some());
    }
}

#[test]
fn srt_stream_keys_with_query_separators_are_refused() {
    let srt = StreamEndpoint::parse_for(StreamProtocol::Srt, "ingest.example:9710").unwrap();
    for secret in ["sk-abcdef&latency=1", "sk-abcdef?mode=caller"] {
        assert_eq!(
            StreamTarget::new(
                stream_target_id(5),
                "SRT".to_owned(),
                StreamProtocol::Srt,
                srt.clone(),
                Some(StreamKey::parse(secret).unwrap()),
                output_id(1),
            ),
            Err(StreamTargetError::InvalidSrtStreamId)
        );
        // An RTMP key is a path segment, where a query string is legitimate.
        assert!(
            StreamTarget::new(
                stream_target_id(5),
                "RTMP".to_owned(),
                StreamProtocol::Rtmp,
                StreamEndpoint::parse("ingest.example/live").unwrap(),
                Some(StreamKey::parse(secret).unwrap()),
                output_id(1),
            )
            .is_ok()
        );
    }
    assert!(
        StreamTarget::new(
            stream_target_id(5),
            "SRT".to_owned(),
            StreamProtocol::Srt,
            srt,
            Some(StreamKey::parse("sk-abcdef=012345").unwrap()),
            output_id(1),
        )
        .is_ok()
    );
}

#[test]
fn stream_key_never_appears_in_debug_display_or_errors() {
    const SECRET: &str = "live-9f3c-secret-key";
    let target = StreamTarget::new(
        stream_target_id(1),
        "Primary".to_owned(),
        StreamProtocol::Rtmps,
        StreamEndpoint::parse("ingest.example:443/live").unwrap(),
        Some(StreamKey::parse(SECRET).unwrap()),
        output_id(1),
    )
    .unwrap()
    .with_backup_endpoint(Some(StreamEndpoint::parse("backup.example/live").unwrap()))
    .unwrap();
    let mut project = valid_project();
    project.add_stream_target_checked(target.clone()).unwrap();

    // The whole project derives Debug, so the containing type has to stay
    // clean too, not just the leaf.
    for rendered in [
        format!("{target:?}"),
        format!("{target}"),
        format!("{:?}", target.key()),
        format!("{project:?}"),
        format!("{:?}", project.stream_targets()),
        target.redacted_url(),
        target.redacted_backup_url().unwrap(),
        StreamKey::parse("ab").unwrap_err().to_string(),
        StreamEndpoint::parse_url(&format!("rtmp://operator:{SECRET}@host/live"))
            .unwrap_err()
            .to_string(),
    ] {
        assert!(
            !rendered.contains(SECRET),
            "stream key leaked into `{rendered}`"
        );
    }

    assert_eq!(
        target.redacted_url(),
        "rtmps://ingest.example:443/live/****"
    );
    assert_eq!(
        target.redacted_backup_url().as_deref(),
        Some("rtmps://backup.example/live/****")
    );
    assert_eq!(
        target.expose_url(),
        format!("rtmps://ingest.example:443/live/{SECRET}")
    );
}

#[test]
fn stream_destinations_author_desired_running_state_off_by_default() {
    let stopped = stream_target(1, "Stopped", output_id(1));
    assert!(!stopped.running());

    let running = stopped.clone().set_running(true);
    assert!(running.running());
    // The setter is builder-style: the original is untouched.
    assert!(!stopped.running());
}

#[test]
fn stream_destinations_author_a_bounded_video_bitrate_with_a_default() {
    // Destinations authored without naming a bitrate assume the default.
    let target = stream_target(1, "Primary", output_id(1));
    assert_eq!(target.video_bitrate_kbps(), DEFAULT_VIDEO_BITRATE_KBPS);
    assert_eq!(DEFAULT_VIDEO_BITRATE_KBPS, 4_500);

    // The bounds are inclusive on both ends.
    let low = target
        .clone()
        .with_video_bitrate(MIN_VIDEO_BITRATE_KBPS)
        .unwrap();
    assert_eq!(low.video_bitrate_kbps(), MIN_VIDEO_BITRATE_KBPS);
    let high = target
        .clone()
        .with_video_bitrate(MAX_VIDEO_BITRATE_KBPS)
        .unwrap();
    assert_eq!(high.video_bitrate_kbps(), MAX_VIDEO_BITRATE_KBPS);

    // Anything outside the bound is refused with a typed error.
    assert_eq!(
        target
            .clone()
            .with_video_bitrate(MIN_VIDEO_BITRATE_KBPS - 1),
        Err(StreamVideoBitrateError::TooLow)
    );
    assert_eq!(
        target.clone().with_video_bitrate(0),
        Err(StreamVideoBitrateError::TooLow)
    );
    assert_eq!(
        target
            .clone()
            .with_video_bitrate(MAX_VIDEO_BITRATE_KBPS + 1),
        Err(StreamVideoBitrateError::TooHigh)
    );
    // The builder setter leaves the original untouched when it refuses.
    assert_eq!(target.video_bitrate_kbps(), DEFAULT_VIDEO_BITRATE_KBPS);
}

#[test]
fn stream_ladder_rungs_validate_bitrates_and_even_bounded_dimensions() {
    // A rung renders dimensions and bitrate, for example `640x360@1000`.
    let rung = ladder_rung(1_000, 640, 360);
    assert_eq!(rung.video_bitrate_kbps(), 1_000);
    assert_eq!(rung.width(), 640);
    assert_eq!(rung.height(), 360);
    assert_eq!(rung.to_string(), "640x360@1000");

    // The bounds are inclusive on both ends.
    for (bitrate, width, height) in [
        (
            MIN_VIDEO_BITRATE_KBPS,
            MIN_STREAM_LADDER_DIMENSION,
            MIN_STREAM_LADDER_DIMENSION,
        ),
        (
            MAX_VIDEO_BITRATE_KBPS,
            MAX_STREAM_LADDER_WIDTH,
            MAX_STREAM_LADDER_HEIGHT,
        ),
    ] {
        assert!(
            StreamLadderRung::new(bitrate, width, height).is_ok(),
            "{width}x{height}@{bitrate} must be accepted"
        );
    }

    for (bitrate, width, height, expected) in [
        (
            MIN_VIDEO_BITRATE_KBPS - 1,
            640,
            360,
            StreamLadderError::BitrateTooLow,
        ),
        (0, 640, 360, StreamLadderError::BitrateTooLow),
        (
            MAX_VIDEO_BITRATE_KBPS + 1,
            640,
            360,
            StreamLadderError::BitrateTooHigh,
        ),
        (
            MIN_VIDEO_BITRATE_KBPS,
            MIN_STREAM_LADDER_DIMENSION - 1,
            360,
            StreamLadderError::DimensionTooSmall,
        ),
        (
            MIN_VIDEO_BITRATE_KBPS,
            640,
            MIN_STREAM_LADDER_DIMENSION - 1,
            StreamLadderError::DimensionTooSmall,
        ),
        (
            MIN_VIDEO_BITRATE_KBPS,
            MAX_STREAM_LADDER_WIDTH + 1,
            360,
            StreamLadderError::DimensionTooLarge,
        ),
        (
            MIN_VIDEO_BITRATE_KBPS,
            640,
            MAX_STREAM_LADDER_HEIGHT + 1,
            StreamLadderError::DimensionTooLarge,
        ),
        (1_000, 641, 360, StreamLadderError::OddDimension),
        (1_000, 640, 361, StreamLadderError::OddDimension),
    ] {
        assert_eq!(
            StreamLadderRung::new(bitrate, width, height),
            Err(expected),
            "{width}x{height}@{bitrate} must be refused"
        );
    }
}

#[test]
fn stream_ladders_require_two_to_eight_increasing_rungs() {
    let low = ladder_rung(1_000, 640, 360);
    let high = ladder_rung(1_500, 1_280, 720);
    let ladder = StreamLadder::new(vec![low, high]).unwrap();
    assert_eq!(ladder.rungs(), &[low, high]);
    assert_eq!(ladder.top_bitrate_kbps(), 1_500);
    assert_eq!(ladder.to_string(), "640x360@1000,1280x720@1500");
    assert_eq!(MIN_STREAM_LADDER_RUNGS, 2);
    assert_eq!(MAX_STREAM_LADDER_RUNGS, 8);

    // One rung is not a ladder, and neither is an empty list.
    assert_eq!(
        StreamLadder::new(Vec::new()),
        Err(StreamLadderError::TooFewRungs)
    );
    assert_eq!(
        StreamLadder::new(vec![low]),
        Err(StreamLadderError::TooFewRungs)
    );

    // The count bound is inclusive: eight increasing rungs are accepted and a
    // ninth is refused before any ordering rule is considered.
    let rungs = |count: u32| {
        (0..count)
            .map(|index| ladder_rung(1_000 + index * 1_000, 640 + index * 16, 360 + index * 16))
            .collect::<Vec<_>>()
    };
    assert!(StreamLadder::new(rungs(8)).is_ok());
    assert_eq!(
        StreamLadder::new(rungs(9)),
        Err(StreamLadderError::TooManyRungs)
    );

    // Bitrates must strictly increase: equal or descending is refused.
    assert_eq!(
        StreamLadder::new(vec![
            ladder_rung(1_000, 640, 360),
            ladder_rung(1_000, 1_280, 720),
        ]),
        Err(StreamLadderError::BitratesNotIncreasing)
    );
    assert_eq!(
        StreamLadder::new(vec![
            ladder_rung(1_500, 1_280, 720),
            ladder_rung(1_000, 640, 360),
        ]),
        Err(StreamLadderError::BitratesNotIncreasing)
    );

    // Dimensions may stay equal but must not shrink at any rung.
    assert!(StreamLadder::new(vec![low, ladder_rung(1_500, 640, 360)]).is_ok());
    assert_eq!(
        StreamLadder::new(vec![
            ladder_rung(1_000, 1_280, 720),
            ladder_rung(1_500, 640, 1_080),
        ]),
        Err(StreamLadderError::DimensionsDecrease)
    );
    assert_eq!(
        StreamLadder::new(vec![
            ladder_rung(1_000, 640, 720),
            ladder_rung(1_500, 640, 360),
        ]),
        Err(StreamLadderError::DimensionsDecrease)
    );
}

#[test]
fn stream_ladders_are_hls_only_and_group_under_the_top_rung() {
    let ladder = StreamLadder::new(vec![
        ladder_rung(1_000, 640, 360),
        ladder_rung(1_500, 1_280, 720),
    ])
    .unwrap();

    let hls = StreamTarget::new(
        stream_target_id(4),
        "Local HLS".to_owned(),
        StreamProtocol::Hls,
        StreamEndpoint::parse_for(StreamProtocol::Hls, "/var/lib/freemix/live").unwrap(),
        None,
        output_id(1),
    )
    .unwrap();
    assert_eq!(hls.ladder(), None);
    assert_eq!(hls.video_bitrate_kbps(), DEFAULT_VIDEO_BITRATE_KBPS);

    // A ladder moves the base bitrate to the top rung, the primary/grouping
    // bitrate a sink advertises, even over one authored earlier.
    let laddered = hls
        .clone()
        .with_video_bitrate(9_000)
        .unwrap()
        .with_ladder(Some(ladder.clone()))
        .unwrap();
    assert_eq!(laddered.ladder(), Some(&ladder));
    assert_eq!(laddered.video_bitrate_kbps(), 1_500);

    // Clearing removes the ladder and leaves the base bitrate where it was.
    let cleared = laddered.clone().with_ladder(None).unwrap();
    assert_eq!(cleared.ladder(), None);
    assert_eq!(cleared.video_bitrate_kbps(), 1_500);

    // RTMP/RTMPS/SRT publish one rendition, so a ladder is refused there.
    let remote = stream_target(1, "Primary", output_id(1));
    assert_eq!(
        remote.clone().with_ladder(Some(ladder)),
        Err(StreamTargetError::LadderNotAllowed)
    );
    // Clearing is a no-op where no ladder was allowed in the first place.
    assert_eq!(remote.clone().with_ladder(None).unwrap(), remote);
    assert_eq!(remote.video_bitrate_kbps(), DEFAULT_VIDEO_BITRATE_KBPS);
}

#[test]
fn recorder_slots_are_one_based_within_bounds_and_round_trip() {
    assert_eq!(RECORDER_SLOT_COUNT, 2);
    assert_eq!(RecorderSlot::ALL.map(RecorderSlot::get), [1, 2]);
    assert_eq!(RecorderSlot::new(1).unwrap().get(), 1);
    assert_eq!(RecorderSlot::new(2).unwrap().get(), 2);
    assert_eq!(RecorderSlot::new(0), Err(RecorderSlotError::OutOfRange));
    assert_eq!(RecorderSlot::new(3), Err(RecorderSlotError::OutOfRange));
    assert_eq!(
        RecorderSlotError::OutOfRange.to_string(),
        "recorder slot must be between 1 and 2"
    );
}

#[test]
fn projects_author_recording_desired_per_slot_off_by_default() {
    let mut project = valid_project();
    assert_eq!(project.recording_desired_slots(), [false, false]);
    for slot in RecorderSlot::ALL {
        assert!(!project.recording_desired(slot));
    }

    project.set_recording_desired(RecorderSlot::new(1).unwrap(), true);
    assert!(project.recording_desired(RecorderSlot::new(1).unwrap()));
    assert!(!project.recording_desired(RecorderSlot::new(2).unwrap()));
    assert_eq!(project.recording_desired_slots(), [true, false]);

    project.set_recording_desired(RecorderSlot::new(2).unwrap(), true);
    assert_eq!(project.recording_desired_slots(), [true, true]);

    let mut recording = Project::new(project_id(9), "Recording", settings())
        .with_recording_desired(RecorderSlot::new(2).unwrap(), true);
    assert_eq!(recording.recording_desired_slots(), [false, true]);
    recording.set_recording_desired(RecorderSlot::new(2).unwrap(), false);
    assert_eq!(recording.recording_desired_slots(), [false, false]);
}

#[test]
fn stinger_slots_are_unique_and_reference_project_inputs() {
    let mut project = valid_project();
    let slot = StingerSlotNumber::new(1).unwrap();
    let config = |media_input| {
        StingerConfig::new(
            slot,
            media_input,
            true,
            12,
            StingerAudioPolicy::MixWithProgram,
            StingerMissingMediaFallback::Fade,
        )
    };
    project.add_stinger(config(input_id(1)));
    assert!(project.validate().is_ok());

    project.add_stinger(config(input_id(99)));
    let errors = project.validate().unwrap_err();
    assert!(errors.iter().any(|error| {
        error.field == "stingers.slot" && error.kind == ValidationErrorKind::DuplicateId
    }));
    assert!(errors.iter().any(|error| {
        error.field == "stingers.media_input"
            && error.kind == ValidationErrorKind::MissingReference(EntityRef::Input(input_id(99)))
    }));
    assert_eq!(StingerSlotNumber::new(0), None);
    assert_eq!(StingerSlotNumber::new(9), None);
}

#[test]
fn stinger_slots_can_be_reconfigured_and_removed_without_duplicates() {
    let mut project = simulated_project();
    let first = StingerConfig::new(
        StingerSlotNumber::new(1).unwrap(),
        input_id(1),
        true,
        12,
        StingerAudioPolicy::Muted,
        StingerMissingMediaFallback::Cut,
    );
    let replacement = StingerConfig::new(
        first.slot,
        input_id(2),
        false,
        24,
        StingerAudioPolicy::MixWithProgram,
        StingerMissingMediaFallback::KeepProgram,
    );

    project.set_stinger(first);
    project.set_stinger(replacement);
    assert_eq!(project.stingers(), &[replacement]);
    assert_eq!(project.remove_stinger(first.slot), Some(replacement));
    assert_eq!(project.stingers(), []);
    assert_eq!(project.remove_stinger(first.slot), None);
    assert_eq!(project.validate(), Ok(()));
}

#[test]
fn set_stinger_checked_rejects_unknown_input_without_mutation() {
    let mut project = valid_project();
    let config = |slot, input, cut| {
        StingerConfig::new(
            StingerSlotNumber::new(slot).unwrap(),
            input_id(input),
            false,
            cut,
            StingerAudioPolicy::Muted,
            StingerMissingMediaFallback::Cut,
        )
    };
    project.set_stinger_checked(config(1, 1, 3)).unwrap();
    let before = project.clone();
    let unknown = config(1, 99, 9);
    assert_eq!(
        project.set_stinger_checked(unknown),
        Err(SetStingerError::UnknownInput(input_id(99)))
    );
    assert_eq!(project, before);

    let second = config(2, 1, 7);
    project.set_stinger_checked(second).unwrap();
    let replacement = config(1, 1, 11);
    project.set_stinger_checked(replacement).unwrap();
    assert_eq!(project.stingers(), &[replacement, second]);
}

#[test]
fn persisted_input_gain_is_exact_and_bounded() {
    assert_eq!(
        InputGainMilliDb::new(InputGainMilliDb::MIN).unwrap().get(),
        -96_000
    );
    assert_eq!(InputGainMilliDb::UNITY.get(), 0);
    assert_eq!(
        InputGainMilliDb::new(InputGainMilliDb::MAX).unwrap().get(),
        24_000
    );
    assert_eq!(InputGainMilliDb::new(InputGainMilliDb::MIN - 1), None);
    assert_eq!(InputGainMilliDb::new(InputGainMilliDb::MAX + 1), None);
    assert_eq!(
        InputBalanceBasisPoints::new(InputBalanceBasisPoints::MIN)
            .unwrap()
            .get(),
        -10_000
    );
    assert_eq!(
        InputBalanceBasisPoints::new(InputBalanceBasisPoints::MAX)
            .unwrap()
            .get(),
        10_000
    );
    assert_eq!(
        InputBalanceBasisPoints::new(InputBalanceBasisPoints::MIN - 1),
        None
    );
    assert_eq!(
        InputBalanceBasisPoints::new(InputBalanceBasisPoints::MAX + 1),
        None
    );
    assert_eq!(InputDelaySamples::new(0), Some(InputDelaySamples::ZERO));
    assert_eq!(
        InputDelaySamples::new(InputDelaySamples::MAX)
            .unwrap()
            .get(),
        48_000
    );
    assert_eq!(InputDelaySamples::new(InputDelaySamples::MAX + 1), None);
    assert_eq!(
        InputAudioStripState::default(),
        InputAudioStripState {
            pan: fm_model::InputPanBasisPoints::CENTER,
            gain: InputGainMilliDb::UNITY,
            balance: InputBalanceBasisPoints::CENTER,
            delay_samples: InputDelaySamples::ZERO,
            muted: false,
            soloed: false,
            follow_video: true,
        }
    );
}

#[test]
fn rectangular_masks_use_half_open_post_crop_source_bounds() {
    let project_with_mask = |mask| {
        let mut project = Project::new(project_id(99), "Mask bounds", settings());
        project.add_input(Input {
            id: input_id(99),
            name: "Source".into(),
            kind: InputKind::Color,
            required_capabilities: Vec::new(),
        });
        let mut masked = layer("Masked", SourceRef::Input(input_id(99)));
        masked.crop = Some(CropRect::new(100, 50, 300, 200));
        masked.mask = Some(mask);
        project.add_scene(Scene {
            id: scene_id(99),
            name: "Masked scene".into(),
            background: Rgba8::OPAQUE_BLACK,
            layers: vec![masked],
            animation: fm_model::SceneAnimation::default(),
        });
        project
    };

    assert_eq!(
        project_with_mask(RectMask::new(299, 199, 1, 1).inverted(true)).validate(),
        Ok(())
    );

    for mask in [
        RectMask::new(0, 0, 0, 1),
        RectMask::new(0, 0, 1, 0),
        RectMask::new(300, 0, 1, 1),
        RectMask::new(0, 200, 1, 1),
        RectMask::new(u32::MAX, 0, 2, 1),
        RectMask::new(0, u32::MAX, 1, 2),
    ] {
        assert!(
            project_with_mask(mask)
                .validate()
                .unwrap_err()
                .iter()
                .any(|error| {
                    error.field == "layers.mask" && error.kind == ValidationErrorKind::OutOfRange
                }),
            "mask {mask:?} was accepted"
        );
    }
}

#[test]
fn layer_effects_persist_ordered_bounded_descriptors() {
    let mut project = Project::new(project_id(99), "Effects", settings());
    project.add_input(Input {
        id: input_id(99),
        name: "Source".into(),
        kind: InputKind::Color,
        required_capabilities: Vec::new(),
    });
    let mut layered = layer("With effects", SourceRef::Input(input_id(99)));
    layered.effects = vec![
        LayerEffect::new("levels", vec![0, 128]),
        LayerEffect::new("brightness_contrast", vec![-20, 35]),
        LayerEffect::new("sharpen", vec![1000]),
    ];
    project.add_scene(Scene {
        id: scene_id(99),
        name: "Effects scene".into(),
        background: Rgba8::OPAQUE_BLACK,
        layers: vec![layered],
        animation: fm_model::SceneAnimation::default(),
    });

    assert_eq!(project.validate(), Ok(()));
    assert_eq!(
        project.scenes()[0].layers[0].effects,
        vec![
            LayerEffect::new("levels", vec![0, 128]),
            LayerEffect::new("brightness_contrast", vec![-20, 35]),
            LayerEffect::new("sharpen", vec![1000]),
        ]
    );
}

#[test]
fn layer_effects_are_bounded_reference_effects() {
    let project_with_effects = |effects| {
        let mut project = Project::new(project_id(99), "Effect bounds", settings());
        project.add_input(Input {
            id: input_id(99),
            name: "Source".into(),
            kind: InputKind::Color,
            required_capabilities: Vec::new(),
        });
        let mut layered = layer("Effects", SourceRef::Input(input_id(99)));
        layered.effects = effects;
        project.add_scene(Scene {
            id: scene_id(99),
            name: "Effects scene".into(),
            background: Rgba8::OPAQUE_BLACK,
            layers: vec![layered],
            animation: fm_model::SceneAnimation::default(),
        });
        project
    };
    let assert_effects_rejected = |effects| {
        let errors = project_with_effects(effects).validate().unwrap_err();
        assert!(
            errors.iter().any(|error| {
                error.field == "layers.effects" && error.kind == ValidationErrorKind::OutOfRange
            }),
            "effects were accepted"
        );
    };

    assert_effects_rejected(
        (0..=LayerEffect::MAX_EFFECTS_PER_LAYER)
            .map(|_| LayerEffect::new("levels", vec![0, 128]))
            .collect::<Vec<_>>(),
    );
    for (effect, error) in [
        (
            LayerEffect::new("", vec![0, 128]),
            LayerEffectError::UnknownEffect,
        ),
        (
            LayerEffect::new("blur", vec![3]),
            LayerEffectError::UnknownEffect,
        ),
        (
            LayerEffect::new("levels", vec![0, 128, 7]),
            LayerEffectError::WrongParameterCount { expected: 2 },
        ),
        (
            LayerEffect::new("levels", vec![0, 256]),
            LayerEffectError::ParameterOutOfRange { parameter: 1 },
        ),
        (
            LayerEffect::new("levels", vec![128, 128]),
            LayerEffectError::InvalidLevels,
        ),
        (
            LayerEffect::new("brightness_contrast", vec![-101, 0]),
            LayerEffectError::ParameterOutOfRange { parameter: 0 },
        ),
        (
            LayerEffect::new("saturation", vec![101]),
            LayerEffectError::ParameterOutOfRange { parameter: 0 },
        ),
        (
            LayerEffect::new("sharpen", vec![-1]),
            LayerEffectError::ParameterOutOfRange { parameter: 0 },
        ),
        (
            LayerEffect::new("flip", vec![0, 2]),
            LayerEffectError::ParameterOutOfRange { parameter: 1 },
        ),
    ] {
        assert_eq!(effect.validate(), Err(error), "{effect:?}");
        assert_effects_rejected(vec![effect]);
    }

    // Every kind's neutral default and range edges are accepted, up to the
    // per-layer bound.
    for name in LayerEffect::KINDS {
        let neutral = LayerEffect::default_of(name).unwrap();
        assert_eq!(neutral.validate(), Ok(()));
        let ranges = LayerEffect::parameter_ranges(name).unwrap();
        assert_eq!(ranges.len(), neutral.parameters.len());
    }
    assert_eq!(LayerEffect::default_of("blur"), None);
    let boundary = vec![
        LayerEffect::new("levels", vec![254, 255]),
        LayerEffect::new("brightness_contrast", vec![-100, 100]),
        LayerEffect::new("saturation", vec![-100]),
        LayerEffect::new("sharpen", vec![1000]),
        LayerEffect::new("flip", vec![1, 1]),
        LayerEffect::new("levels", vec![0, 1]),
        LayerEffect::new("saturation", vec![100]),
        LayerEffect::new("sharpen", vec![0]),
    ];
    assert_eq!(boundary.len(), LayerEffect::MAX_EFFECTS_PER_LAYER);
    assert_eq!(project_with_effects(boundary).validate(), Ok(()));
}

#[test]
fn layer_keys_are_bounded_and_set_atomically() {
    let mut project = Project::new(project_id(99), "Keys", settings());
    project.add_input(Input {
        id: input_id(99),
        name: "Source".into(),
        kind: InputKind::Color,
        required_capabilities: Vec::new(),
    });
    project.add_scene(Scene {
        id: scene_id(99),
        name: "Keys scene".into(),
        background: Rgba8::OPAQUE_BLACK,
        layers: vec![layer("Keyed", SourceRef::Input(input_id(99)))],
        animation: fm_model::SceneAnimation::default(),
    });
    let scene = scene_id(99);
    let chroma = LayerKey::Chroma(ChromaLayerKey::GREEN);
    project.set_scene_layer_key(scene, 0, Some(chroma)).unwrap();
    assert_eq!(project.scenes()[0].layers[0].key, Some(chroma));
    assert_eq!(project.validate(), Ok(()));

    let empty_band = LayerKey::Luma(LumaLayerKey {
        low: 200,
        high: 200,
        softness: 0,
        invert: false,
        refinement: fm_model::KeyRefinement::NONE,
    });
    assert_eq!(
        project.set_scene_layer_key(scene, 0, Some(empty_band)),
        Err(SceneLayerError::InvalidKey {
            scene,
            index: 0,
            reason: LayerKeyError::EmptyLumaBand,
        })
    );
    assert_eq!(project.scenes()[0].layers[0].key, Some(chroma));
    assert!(project.set_scene_layer_key(scene, 1, None).is_err());
    assert!(project.set_scene_layer_key(scene_id(1), 0, None).is_err());

    let luma = LayerKey::Luma(LumaLayerKey {
        high: 235,
        invert: true,
        ..LumaLayerKey::BLACK
    });
    project.set_scene_layer_key(scene, 0, Some(luma)).unwrap();
    assert_eq!(project.scenes()[0].layers[0].key, Some(luma));
    project.set_scene_layer_key(scene, 0, None).unwrap();
    assert_eq!(project.scenes()[0].layers[0].key, None);

    // Validation reports a stored invalid key.
    let mut broken = project.scenes()[0].clone();
    broken.layers[0].key = Some(empty_band);
    let mut invalid = Project::new(project_id(98), "Broken key", settings());
    invalid.add_input(project.inputs()[0].clone());
    invalid.add_scene(broken);
    let errors = invalid.validate().unwrap_err();
    assert!(
        errors
            .iter()
            .any(|error| error.field == "layers.key"
                && error.kind == ValidationErrorKind::OutOfRange)
    );
}

#[allow(clippy::too_many_lines)] // one story: every key bound and the key input's hold
#[test]
fn key_fill_keys_and_refinements_are_validated_and_hold_their_key_input() {
    let mut project = Project::new(project_id(97), "Key fill", settings());
    for (id, name, kind) in [
        (95, "Fill", InputKind::Color),
        (96, "Key", InputKind::Color),
        (
            97,
            "Nested",
            InputKind::Scene {
                scene_id: scene_id(96),
                audio_source: None,
            },
        ),
        (
            98,
            "Nested alias",
            InputKind::Alias {
                target: input_id(97),
                crop: None,
            },
        ),
    ] {
        project.add_input(Input {
            id: input_id(id),
            name: name.into(),
            kind,
            required_capabilities: Vec::new(),
        });
    }
    for (id, name) in [(97, "Keyed"), (96, "Other")] {
        project.add_scene(Scene {
            id: scene_id(id),
            name: name.into(),
            background: Rgba8::OPAQUE_BLACK,
            layers: vec![layer("Fill", SourceRef::Input(input_id(95)))],
            animation: fm_model::SceneAnimation::default(),
        });
    }
    let scene = scene_id(97);
    let fill = fm_model::FillLayerKey {
        channel: fm_model::KeyChannel::Alpha,
        refinement: fm_model::KeyRefinement {
            clip_black: 10,
            clip_white: 240,
            choke: -8,
            edge_softness: 8,
            despill: fm_model::KeyDespill::Blue { strength: 200 },
        },
        ..fm_model::FillLayerKey::linear(input_id(96))
    };
    project
        .set_scene_layer_key(scene, 0, Some(LayerKey::Fill(fill)))
        .unwrap();
    assert_eq!(project.validate(), Ok(()));
    // The key input is held like a layer source.
    assert_eq!(
        project.input_reference(input_id(96)),
        Some(fm_model::InputReference::SceneLayer(scene))
    );

    // A missing or Scene key input is refused; the layer keeps its key.
    assert_eq!(
        project.set_scene_layer_key(
            scene,
            0,
            Some(LayerKey::Fill(fm_model::FillLayerKey::linear(input_id(5))))
        ),
        Err(SceneLayerError::MissingInput(input_id(5)))
    );
    assert_eq!(
        project.set_scene_layer_key(
            scene,
            0,
            Some(LayerKey::Fill(fm_model::FillLayerKey::linear(input_id(97))))
        ),
        Err(SceneLayerError::SceneKeyInput(input_id(97)))
    );
    // An alias renders its target: an alias of a Scene input is refused too.
    assert_eq!(
        project.set_scene_layer_key(
            scene,
            0,
            Some(LayerKey::Fill(fm_model::FillLayerKey::linear(input_id(98))))
        ),
        Err(SceneLayerError::SceneKeyInput(input_id(98)))
    );
    assert_eq!(
        project.scenes()[0].layers[0].key,
        Some(LayerKey::Fill(fill))
    );

    // Refinement bounds hold for every key kind.
    for (refinement, reason) in [
        (
            fm_model::KeyRefinement {
                clip_black: 50,
                clip_white: 50,
                ..fm_model::KeyRefinement::NONE
            },
            LayerKeyError::EmptyClipRange,
        ),
        (
            fm_model::KeyRefinement {
                choke: 9,
                ..fm_model::KeyRefinement::NONE
            },
            LayerKeyError::ChokeOutOfRange,
        ),
        (
            fm_model::KeyRefinement {
                choke: -9,
                ..fm_model::KeyRefinement::NONE
            },
            LayerKeyError::ChokeOutOfRange,
        ),
        (
            fm_model::KeyRefinement {
                edge_softness: 9,
                ..fm_model::KeyRefinement::NONE
            },
            LayerKeyError::EdgeSoftnessOutOfRange,
        ),
    ] {
        for key in [
            LayerKey::Chroma(ChromaLayerKey::GREEN),
            LayerKey::Luma(LumaLayerKey::BLACK),
            LayerKey::Fill(fill),
        ] {
            assert_eq!(
                project.set_scene_layer_key(scene, 0, Some(key.with_refinement(refinement))),
                Err(SceneLayerError::InvalidKey {
                    scene,
                    index: 0,
                    reason,
                })
            );
        }
    }
    let empty_band = fm_model::FillLayerKey {
        low: 100,
        high: 100,
        ..fill
    };
    assert_eq!(
        LayerKey::Fill(empty_band).validate(),
        Err(LayerKeyError::EmptyLumaBand)
    );

    // Validation reports a stored key input that is gone or a Scene input.
    for (key_input, kind) in [
        (
            input_id(5),
            ValidationErrorKind::MissingReference(fm_model::EntityRef::Input(input_id(5))),
        ),
        (input_id(97), ValidationErrorKind::OutOfRange),
        (input_id(98), ValidationErrorKind::OutOfRange),
    ] {
        let mut broken = Project::new(project_id(96), "Broken key input", settings());
        for input in project.inputs() {
            broken.add_input(input.clone());
        }
        let mut keyed = project.scenes()[0].clone();
        keyed.layers[0].key = Some(LayerKey::Fill(fm_model::FillLayerKey { key_input, ..fill }));
        broken.add_scene(keyed);
        broken.add_scene(project.scenes()[1].clone());
        let errors = broken.validate().unwrap_err();
        assert!(
            errors
                .iter()
                .any(|error| error.field == "layers.key.key_input" && error.kind == kind),
            "{errors:?}"
        );
    }
}

#[test]
fn scene_layer_effect_mutations_are_bounded_and_atomic() {
    let mut project = Project::new(project_id(99), "Effect mutations", settings());
    project.add_input(Input {
        id: input_id(99),
        name: "Source".into(),
        kind: InputKind::Color,
        required_capabilities: Vec::new(),
    });
    let mut layered = layer("Effects", SourceRef::Input(input_id(99)));
    layered.effects = vec![LayerEffect::new("levels", vec![0, 128])];
    project.add_scene(Scene {
        id: scene_id(99),
        name: "Effects scene".into(),
        background: Rgba8::OPAQUE_BLACK,
        layers: vec![layered],
        animation: fm_model::SceneAnimation::default(),
    });
    let scene = scene_id(99);

    let effects = |project: &Project| project.scenes()[0].layers[0].effects.clone();
    project
        .add_scene_layer_effect(scene, 0, LayerEffect::new("saturation", vec![-50]))
        .unwrap();
    assert_eq!(
        effects(&project),
        vec![
            LayerEffect::new("levels", vec![0, 128]),
            LayerEffect::new("saturation", vec![-50]),
        ]
    );
    project
        .set_scene_layer_effects(scene, 0, vec![LayerEffect::new("flip", vec![0, 1])])
        .unwrap();
    assert_eq!(
        effects(&project),
        vec![LayerEffect::new("flip", vec![0, 1])]
    );
    project.remove_scene_layer_effect(scene, 0, 0).unwrap();
    assert_eq!(effects(&project), [] as [fm_model::LayerEffect; 0]);

    let full = (0..LayerEffect::MAX_EFFECTS_PER_LAYER)
        .map(|_| LayerEffect::new("levels", vec![0, 128]))
        .collect::<Vec<_>>();
    project
        .set_scene_layer_effects(scene, 0, full.clone())
        .unwrap();
    for error in [
        project.add_scene_layer_effect(scene, 0, LayerEffect::new("flip", vec![1, 0])),
        project.set_scene_layer_effects(
            scene,
            0,
            (0..=LayerEffect::MAX_EFFECTS_PER_LAYER)
                .map(|_| LayerEffect::new("levels", vec![0, 128]))
                .collect(),
        ),
        project.set_scene_layer_effects(scene, 0, vec![LayerEffect::new("", Vec::new())]),
        project.set_scene_layer_effects(
            scene,
            0,
            vec![LayerEffect::new("levels", vec![0, 128, 255])],
        ),
        project.remove_scene_layer_effect(scene, 0, LayerEffect::MAX_EFFECTS_PER_LAYER),
        project.set_scene_layer_effects(scene_id(1), 0, Vec::new()),
        project.set_scene_layer_effects(scene, 1, Vec::new()),
        project.add_scene_layer_effect(scene, 1, LayerEffect::new("levels", vec![0, 128])),
        project.remove_scene_layer_effect(scene, 1, 0),
    ] {
        assert!(error.is_err());
        assert_eq!(effects(&project), full, "a rejection mutated the layer");
    }
    assert_eq!(
        project.add_scene_layer_effect(scene, 0, LayerEffect::new("flip", vec![1, 0])),
        Err(SceneLayerError::TooManyEffects { scene, index: 0 })
    );
    assert_eq!(
        project.remove_scene_layer_effect(scene, 0, LayerEffect::MAX_EFFECTS_PER_LAYER),
        Err(SceneLayerError::EffectIndexOutOfRange {
            scene,
            index: 0,
            effect_index: LayerEffect::MAX_EFFECTS_PER_LAYER,
            length: LayerEffect::MAX_EFFECTS_PER_LAYER,
        })
    );
}

#[test]
fn dangling_references_and_malformed_capabilities_are_reported_together() {
    let mut project = valid_project();
    project.add_output(Output {
        id: output_id(2),
        name: "Broken".into(),
        video_source: scene_id(99),
        audio_source: bus_id(99),
        startup: StartupPolicy::Stopped,
        required_capabilities: vec!["GPU invalid".into()],
    });

    let errors = project.validate().unwrap_err();
    assert!(errors.iter().any(|error| {
        error.kind == ValidationErrorKind::MissingReference(EntityRef::Scene(scene_id(99)))
    }));
    assert!(errors.iter().any(|error| {
        error.kind == ValidationErrorKind::MissingReference(EntityRef::AudioBus(bus_id(99)))
    }));
    assert!(
        errors
            .iter()
            .any(|error| error.kind == ValidationErrorKind::MalformedCapabilityKey)
    );
}

#[test]
fn identifiers_and_names_must_be_unique_per_collection() {
    let mut project = valid_project();
    project.add_input(Input {
        id: input_id(2),
        name: "camera".into(),
        kind: InputKind::Color,
        required_capabilities: Vec::new(),
    });
    assert_eq!(project.validate(), Ok(()));

    project.add_input(Input {
        id: input_id(1),
        name: "Camera copy".into(),
        kind: InputKind::Color,
        required_capabilities: Vec::new(),
    });
    project.add_input(Input {
        id: input_id(3),
        name: "Camera".into(),
        kind: InputKind::Color,
        required_capabilities: Vec::new(),
    });
    project.add_input(Input {
        id: input_id(4),
        name: "x".repeat(MAX_INPUT_NAME_BYTES + 1),
        kind: InputKind::Color,
        required_capabilities: Vec::new(),
    });

    let errors = project.validate().unwrap_err();
    assert!(
        errors
            .iter()
            .any(|error| error.kind == ValidationErrorKind::DuplicateId)
    );
    assert!(errors.iter().any(|error| {
        error.entity == Some(EntityRef::Input(input_id(3)))
            && error.kind == ValidationErrorKind::DuplicateName
    }));
    assert!(errors.iter().any(|error| {
        error.entity == Some(EntityRef::Input(input_id(4)))
            && error.field == "name"
            && error.kind == ValidationErrorKind::OutOfRange
    }));
}

#[test]
fn nested_scene_cycles_are_rejected() {
    let mut project = valid_project();
    project.add_scene(Scene {
        id: scene_id(2),
        name: "Nested A".into(),
        background: Rgba8::OPAQUE_BLACK,
        layers: vec![layer("B", SourceRef::Scene(scene_id(3)))],
        animation: fm_model::SceneAnimation::default(),
    });
    project.add_scene(Scene {
        id: scene_id(3),
        name: "Nested B".into(),
        background: Rgba8::OPAQUE_BLACK,
        layers: vec![layer("A", SourceRef::Scene(scene_id(2)))],
        animation: fm_model::SceneAnimation::default(),
    });

    let errors = project.validate().unwrap_err();
    assert_eq!(
        errors
            .iter()
            .filter(|error| error.kind == ValidationErrorKind::Cycle)
            .count(),
        2
    );
}

#[test]
fn scene_inputs_validate_full_width_scene_and_audio_references() {
    let high = u128::from(u64::MAX) + 100;
    let mut project = Project::new(project_id(high), "Scene routing", settings());
    project.add_input(Input {
        id: input_id(high + 1),
        name: "Audio".into(),
        kind: InputKind::Color,
        required_capabilities: Vec::new(),
    });
    project.add_input(Input {
        id: input_id(high + 2),
        name: "Scene input".into(),
        kind: InputKind::Scene {
            scene_id: scene_id(high + 3),
            audio_source: Some(input_id(high + 1)),
        },
        required_capabilities: Vec::new(),
    });
    project.add_scene(Scene {
        id: scene_id(high + 3),
        name: "Scene".into(),
        background: Rgba8::new(8, 4, 2, 16),
        layers: Vec::new(),
        animation: fm_model::SceneAnimation::default(),
    });

    assert_eq!(project.validate(), Ok(()));
}

#[test]
fn scene_input_missing_scene_and_audio_references_are_reported() {
    let mut project = Project::new(project_id(9), "Missing scene routes", settings());
    project.add_input(Input {
        id: input_id(9),
        name: "Broken scene input".into(),
        kind: InputKind::Scene {
            scene_id: scene_id(99),
            audio_source: Some(input_id(98)),
        },
        required_capabilities: Vec::new(),
    });

    let errors = project.validate().unwrap_err();
    assert!(errors.iter().any(|error| {
        error.kind == ValidationErrorKind::MissingReference(EntityRef::Scene(scene_id(99)))
    }));
    assert!(errors.iter().any(|error| {
        error.kind == ValidationErrorKind::MissingReference(EntityRef::Input(input_id(98)))
    }));
}

#[test]
fn scene_removal_checks_references_in_order_without_mutation() {
    let mut project = valid_project();
    project.add_scene(Scene {
        id: scene_id(2),
        name: "Close".into(),
        background: Rgba8::OPAQUE_BLACK,
        layers: Vec::new(),
        animation: fm_model::SceneAnimation::default(),
    });
    project.add_scene(Scene {
        id: scene_id(3),
        name: "Wide 2".into(),
        background: Rgba8::OPAQUE_BLACK,
        layers: vec![layer("Close", SourceRef::Scene(scene_id(2)))],
        animation: fm_model::SceneAnimation::default(),
    });
    project.add_input(Input {
        id: input_id(2),
        name: "Close input".into(),
        kind: InputKind::Scene {
            scene_id: scene_id(2),
            audio_source: None,
        },
        required_capabilities: Vec::new(),
    });
    project
        .set_output_route(output_id(1), scene_id(2), bus_id(1))
        .unwrap();
    let original = project.clone();
    assert_eq!(
        project.remove_scene(scene_id(2)),
        Err(RemoveSceneError::InputReference {
            input: input_id(2),
            scene: scene_id(2)
        })
    );
    assert_eq!(project, original);
    project.remove_input(input_id(2)).unwrap();
    assert_eq!(
        project.remove_scene(scene_id(2)),
        Err(RemoveSceneError::LayerReference {
            owner: scene_id(3),
            source: scene_id(2)
        })
    );
    assert_eq!(
        project
            .scenes()
            .iter()
            .map(|scene| scene.id)
            .collect::<Vec<_>>(),
        vec![scene_id(1), scene_id(2), scene_id(3)]
    );
    project
        .set_scene_layer_source(scene_id(3), 0, SourceRef::Input(input_id(1)))
        .unwrap();
    assert_eq!(
        project.remove_scene(scene_id(2)),
        Err(RemoveSceneError::OutputReference {
            output: output_id(1),
            scene: scene_id(2)
        })
    );
    project
        .set_output_route(output_id(1), scene_id(1), bus_id(1))
        .unwrap();
    project.remove_scene(scene_id(2)).unwrap();
    assert_eq!(
        project
            .scenes()
            .iter()
            .map(|scene| scene.id)
            .collect::<Vec<_>>(),
        vec![scene_id(1), scene_id(3)]
    );
    assert_eq!(
        project.remove_scene(scene_id(99)),
        Err(RemoveSceneError::UnknownScene(scene_id(99)))
    );
}

#[test]
fn checked_output_creation_and_route_validate_references_without_mutation() {
    let mut project = valid_project();
    project.add_scene(Scene {
        id: scene_id(2),
        name: "Close".into(),
        background: Rgba8::OPAQUE_BLACK,
        layers: Vec::new(),
        animation: fm_model::SceneAnimation::default(),
    });
    project
        .add_audio_bus_checked(AudioBus {
            id: bus_id(2),
            name: "Aux".into(),
            sends: Vec::new(),
        })
        .unwrap();
    let before_empty_bus = project.clone();
    let mut invalid_bus = project.audio_buses()[0].clone();
    invalid_bus.id = bus_id(3);
    invalid_bus.name = " \t ".into();
    assert_eq!(
        project.add_audio_bus_checked(invalid_bus),
        Err(fm_model::AddAudioBusError::EmptyName)
    );
    assert_eq!(project, before_empty_bus);
    let buses_before_duplicate = project.audio_buses().to_vec();
    assert!(matches!(
        project.add_audio_bus_checked(AudioBus {
            id: bus_id(3),
            name: "mAsTeR".into(),
            sends: Vec::new(),
        }),
        Err(fm_model::AddAudioBusError::DuplicateName)
    ));
    assert_eq!(project.audio_buses(), buses_before_duplicate);
    project
        .add_output_checked(Output {
            id: output_id(2),
            name: "Auxiliary".into(),
            video_source: scene_id(2),
            audio_source: bus_id(2),
            startup: StartupPolicy::Stopped,
            required_capabilities: Vec::new(),
        })
        .unwrap();
    let before_empty_output = project.clone();
    let mut invalid_output = project.outputs()[0].clone();
    invalid_output.id = output_id(3);
    invalid_output.name = " \n ".into();
    assert_eq!(
        project.add_output_checked(invalid_output),
        Err(fm_model::AddOutputError::EmptyName)
    );
    assert_eq!(project, before_empty_output);
    let outputs_before_duplicate = project.outputs().to_vec();
    assert!(matches!(
        project.add_output_checked(Output {
            id: output_id(3),
            name: "pRoGrAm".into(),
            video_source: scene_id(2),
            audio_source: bus_id(2),
            startup: StartupPolicy::Stopped,
            required_capabilities: Vec::new(),
        }),
        Err(fm_model::AddOutputError::DuplicateName)
    ));
    assert_eq!(project.outputs(), outputs_before_duplicate);
    let before = project.outputs().to_vec();
    project
        .set_output_route(output_id(1), scene_id(2), bus_id(2))
        .unwrap();
    assert_eq!(project.outputs()[0].id, before[0].id);
    assert_eq!(project.outputs()[0].name, before[0].name);
    assert_eq!(project.outputs()[0].startup, before[0].startup);
    assert_eq!(
        project.outputs()[0].required_capabilities,
        before[0].required_capabilities
    );
    assert_eq!(project.outputs()[0].video_source, scene_id(2));
    assert_eq!(project.outputs()[0].audio_source, bus_id(2));

    let unchanged = project.outputs().to_vec();
    assert!(matches!(
        project.set_output_route(output_id(99), scene_id(99), bus_id(99)),
        Err(fm_model::SetOutputRouteError::UnknownOutput(output)) if output == output_id(99)
    ));
    assert_eq!(project.outputs(), unchanged);
    assert!(matches!(
        project.set_output_route(output_id(1), scene_id(99), bus_id(2)),
        Err(fm_model::SetOutputRouteError::UnknownScene(scene)) if scene == scene_id(99)
    ));
    assert!(matches!(
        project.set_output_route(output_id(1), scene_id(2), bus_id(99)),
        Err(fm_model::SetOutputRouteError::UnknownBus(bus)) if bus == bus_id(99)
    ));
    assert_eq!(project.outputs(), unchanged);
}

#[test]
fn scene_composition_value_bounds_and_premultiplication_are_validated() {
    let mut project = Project::new(project_id(10), "Bounds", settings());
    project.add_input(Input {
        id: input_id(1),
        name: "Source".into(),
        kind: InputKind::Color,
        required_capabilities: Vec::new(),
    });
    let mut invalid_layer = layer("Source", SourceRef::Input(input_id(1)));
    invalid_layer.geometry.width = 0;
    invalid_layer.crop = Some(CropRect::new(1919, 0, 2, 1));
    project.add_scene(Scene {
        id: scene_id(1),
        name: "Invalid".into(),
        background: Rgba8::new(2, 0, 0, 1),
        layers: vec![invalid_layer],
        animation: fm_model::SceneAnimation::default(),
    });

    let errors = project.validate().unwrap_err();
    for field in ["background", "layers.geometry", "layers.crop"] {
        assert!(errors.iter().any(|error| error.field == field));
    }

    // A stored tilt is below half a quarter turn.
    let mut tilted = layer("Source", SourceRef::Input(input_id(1)));
    tilted.geometry = tilted.geometry.with_tilt(45_000);
    let mut project = Project::new(project_id(12), "Tilted", settings());
    project.add_input(Input {
        id: input_id(1),
        name: "Source".into(),
        kind: InputKind::Color,
        required_capabilities: Vec::new(),
    });
    project.add_scene(Scene {
        id: scene_id(1),
        name: "Tilted".into(),
        background: Rgba8::new(0, 0, 0, 255),
        layers: vec![tilted],
        animation: fm_model::SceneAnimation::default(),
    });
    let errors = project.validate().unwrap_err();
    assert!(errors.iter().any(|error| error.field == "layers.geometry"));
}

#[test]
fn persisted_scenes_can_exceed_the_renderer_execution_limit() {
    let mut project = Project::new(project_id(11), "Large stored scene", settings());
    project.add_input(Input {
        id: input_id(1),
        name: "Source".into(),
        kind: InputKind::Color,
        required_capabilities: Vec::new(),
    });
    let layers = (0..=Scene::MAX_RENDERED_LAYERS)
        .map(|index| layer(&format!("Layer {index}"), SourceRef::Input(input_id(1))))
        .collect();
    project.add_scene(Scene {
        id: scene_id(1),
        name: "Preserved".into(),
        background: Rgba8::OPAQUE_BLACK,
        layers,
        animation: fm_model::SceneAnimation::default(),
    });

    assert_eq!(project.validate(), Ok(()));
}

/// The offline layer edits (and so their `freemix-cli scene-layer-*`
/// commands) refuse to take a scene past the compositor's enabled-layer
/// limit, while a stored scene beyond it still loads.
#[test]
fn offline_layer_edits_refuse_more_enabled_layers_than_the_compositor_draws() {
    let mut project = Project::new(project_id(12), "Full scene", settings());
    project.add_input(Input {
        id: input_id(1),
        name: "Source".into(),
        kind: InputKind::Color,
        required_capabilities: Vec::new(),
    });
    let source = SourceRef::Input(input_id(1));
    project.add_scene(Scene {
        id: scene_id(1),
        name: "Full".into(),
        background: Rgba8::OPAQUE_BLACK,
        layers: (0..Scene::MAX_RENDERED_LAYERS)
            .map(|index| layer(&format!("Layer {index}"), source))
            .collect(),
        animation: fm_model::SceneAnimation::default(),
    });
    let before = project.clone();
    assert_eq!(
        project.add_layer_to_scene(scene_id(1), layer("One more", source)),
        Err(AddSceneLayerError::TooManyEnabledLayers)
    );
    assert_eq!(
        project.duplicate_scene_layer(scene_id(1), 0, "Copy".into()),
        Err(SceneLayerError::TooManyEnabledLayers { scene: scene_id(1) })
    );
    assert_eq!(project, before);
    // A disabled layer may be added; enabling it is refused.
    let mut hidden = layer("Hidden", source);
    hidden.enabled = false;
    project.add_layer_to_scene(scene_id(1), hidden).unwrap();
    let hidden_index = Scene::MAX_RENDERED_LAYERS;
    let refused = project.set_scene_layer_appearance(scene_id(1), hidden_index, true, 255);
    assert_eq!(
        refused,
        Err(SceneLayerError::TooManyEnabledLayers { scene: scene_id(1) })
    );
    assert!(
        refused
            .unwrap_err()
            .to_string()
            .contains("64 enabled layers")
    );
    // Disabling another layer makes room.
    project
        .set_scene_layer_appearance(scene_id(1), 0, false, 255)
        .unwrap();
    project
        .set_scene_layer_appearance(scene_id(1), hidden_index, true, 255)
        .unwrap();
}

#[test]
fn scene_input_video_and_audio_cycles_are_rejected() {
    let mut project = Project::new(project_id(20), "Cycles", settings());
    project.add_input(Input {
        id: input_id(20),
        name: "A".into(),
        kind: InputKind::Scene {
            scene_id: scene_id(20),
            audio_source: Some(input_id(21)),
        },
        required_capabilities: Vec::new(),
    });
    project.add_input(Input {
        id: input_id(21),
        name: "B".into(),
        kind: InputKind::Scene {
            scene_id: scene_id(21),
            audio_source: Some(input_id(20)),
        },
        required_capabilities: Vec::new(),
    });
    project.add_scene(Scene {
        id: scene_id(20),
        name: "A".into(),
        background: Rgba8::OPAQUE_BLACK,
        layers: vec![layer("B", SourceRef::Input(input_id(21)))],
        animation: fm_model::SceneAnimation::default(),
    });
    project.add_scene(Scene {
        id: scene_id(21),
        name: "B".into(),
        background: Rgba8::OPAQUE_BLACK,
        layers: vec![layer("A", SourceRef::Input(input_id(20)))],
        animation: fm_model::SceneAnimation::default(),
    });

    let errors = project.validate().unwrap_err();
    assert_eq!(
        errors
            .iter()
            .filter(|error| error.kind == ValidationErrorKind::Cycle)
            .count(),
        4
    );
}

#[test]
fn audio_bus_cycles_are_rejected() {
    let mut project = valid_project();
    project.add_audio_bus(AudioBus {
        id: bus_id(2),
        name: "Aux A".into(),
        sends: vec![BusSend {
            destination: bus_id(3),
        }],
    });
    project.add_audio_bus(AudioBus {
        id: bus_id(3),
        name: "Aux B".into(),
        sends: vec![BusSend {
            destination: bus_id(2),
        }],
    });

    let errors = project.validate().unwrap_err();
    assert_eq!(
        errors
            .iter()
            .filter(|error| error.kind == ValidationErrorKind::Cycle)
            .count(),
        2
    );
}

#[test]
fn acyclic_scene_and_bus_graphs_with_shared_descendants_validate_promptly() {
    // Each scene composites the next scene twice and each bus sends to the
    // next two buses, so path-by-path exploration would revisit shared
    // descendants exponentially often. Reachability must stay polynomial.
    const DEPTH: u128 = 64;
    let mut project = Project::new(project_id(30), "Wide graph", settings());
    for index in 1..=DEPTH {
        let layers = if index == DEPTH {
            Vec::new()
        } else {
            vec![
                layer("Left", SourceRef::Scene(scene_id(index + 1))),
                layer("Right", SourceRef::Scene(scene_id(index + 1))),
            ]
        };
        project.add_scene(Scene {
            id: scene_id(index),
            name: format!("Scene {index}"),
            background: Rgba8::OPAQUE_BLACK,
            layers,
            animation: fm_model::SceneAnimation::default(),
        });
        project.add_audio_bus(AudioBus {
            id: bus_id(index),
            name: format!("Bus {index}"),
            sends: [index + 1, index + 2]
                .into_iter()
                .filter(|destination| *destination <= DEPTH)
                .map(|destination| BusSend {
                    destination: bus_id(destination),
                })
                .collect(),
        });
    }

    assert_eq!(project.validate(), Ok(()));
}

#[test]
fn long_reference_chains_validate_without_deep_recursion() {
    // A decoded project.json can chain thousands of scenes, scene-input audio
    // sources, and bus sends. Cycle detection must neither recurse once per
    // link (overflowing a small stack) nor rescan the graph per node.
    const LENGTH: u128 = 4_096;
    let mut project = Project::new(project_id(40), "Long chains", settings());
    for index in 1..=LENGTH {
        let next = index % LENGTH + 1;
        project.add_scene(Scene {
            id: scene_id(index),
            name: format!("Scene {index}"),
            background: Rgba8::OPAQUE_BLACK,
            layers: vec![layer("Next", SourceRef::Scene(scene_id(next)))],
            animation: fm_model::SceneAnimation::default(),
        });
        project.add_input(Input {
            id: input_id(index),
            name: format!("Scene input {index}"),
            kind: InputKind::Scene {
                scene_id: scene_id(index),
                audio_source: Some(input_id(next)),
            },
            required_capabilities: Vec::new(),
        });
        project.add_audio_bus(AudioBus {
            id: bus_id(index),
            name: format!("Bus {index}"),
            sends: vec![BusSend {
                destination: bus_id(next),
            }],
        });
    }

    let cycles = std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(move || {
            project
                .validate()
                .unwrap_err()
                .into_iter()
                .filter(|error| error.kind == ValidationErrorKind::Cycle)
                .count()
        })
        .unwrap()
        .join()
        .unwrap();
    assert_eq!(cycles, 3 * usize::try_from(LENGTH).unwrap());
}

#[test]
fn edit_locks_are_project_state_validated_against_inputs() {
    let mut project = valid_project();
    assert!(!project.project_locked());
    assert!(project.locked_inputs().is_empty());

    project.set_project_locked(true);
    project.set_input_locked_checked(input_id(1), true).unwrap();
    assert!(project.project_locked());
    assert!(project.is_input_locked(input_id(1)));
    assert_eq!(
        project.locked_inputs().iter().copied().collect::<Vec<_>>(),
        vec![input_id(1)]
    );
    assert_eq!(project.validate(), Ok(()));

    let before = project.clone();
    assert_eq!(
        project.set_input_locked_checked(input_id(9), true),
        Err(SetInputLockError::UnknownInput(input_id(9)))
    );
    assert_eq!(project, before);

    project.set_input_locked(input_id(9), true);
    assert_eq!(
        project.validate(),
        Err(vec![ValidationError {
            entity: Some(EntityRef::Input(input_id(9))),
            field: "locked_inputs",
            kind: ValidationErrorKind::MissingReference(EntityRef::Input(input_id(9))),
        }])
    );

    project.set_input_locked(input_id(9), false);
    project
        .set_input_locked_checked(input_id(1), false)
        .unwrap();
    project.set_project_locked(false);
    assert!(!project.project_locked());
    assert!(project.locked_inputs().is_empty());
    assert_eq!(project.validate(), Ok(()));
}

#[test]
fn removing_a_locked_input_drops_its_lock() {
    let mut project = valid_project();
    project.add_input(Input {
        id: input_id(2),
        name: "Spare".into(),
        kind: InputKind::Simulated(SimulatedInput::new(
            SimulatedVideo::Bars,
            SimulatedAudio::Silence,
        )),
        required_capabilities: Vec::new(),
    });
    project.set_input_locked_checked(input_id(1), true).unwrap();
    project.set_input_locked_checked(input_id(2), true).unwrap();

    project.remove_input(input_id(2)).unwrap();

    assert!(!project.is_input_locked(input_id(2)));
    assert!(project.is_input_locked(input_id(1)));
    assert_eq!(project.validate(), Ok(()));
}

fn title_field(id: u128, value: fm_model::TitleFieldValue) -> fm_model::TitleFieldEntry {
    fm_model::TitleFieldEntry {
        id: fm_model::TitleFieldId::new(NonZeroU128::new(id).unwrap()),
        value,
        revision: 1,
    }
}

fn title_project(fields: Vec<fm_model::TitleFieldEntry>) -> Project {
    let mut project = valid_project();
    project.add_input(Input {
        id: input_id(9),
        name: "Lower third".into(),
        kind: InputKind::Title(fm_model::TitleInput {
            template_uri: "asset://titles/lower-third.fmtitle".into(),
            field_revision: 3,
            fields,
        }),
        required_capabilities: Vec::new(),
    });
    project
}

fn title_errors(project: &Project) -> Vec<(&'static str, ValidationErrorKind)> {
    project
        .validate()
        .err()
        .unwrap_or_default()
        .into_iter()
        .map(|error| {
            assert_eq!(error.entity, Some(EntityRef::Input(input_id(9))));
            (error.field, error.kind)
        })
        .collect()
}

#[test]
fn title_inputs_validate_every_field_kind_and_stay_leaves() {
    use fm_model::{
        AudioRoute, MixFeedHistory, MixSelection, RenderSource, SourceResolver, TitleFieldValue,
    };

    let project = title_project(vec![
        title_field(1, TitleFieldValue::Text("Jane Doe".into())),
        title_field(2, TitleFieldValue::Number(-4)),
        title_field(
            3,
            TitleFieldValue::Color(Rgba8::new(0x11, 0x22, 0x33, 0xff)),
        ),
        title_field(
            4,
            TitleFieldValue::Image {
                asset_uri: "asset://images/logo.png".into(),
            },
        ),
    ]);
    project.validate().unwrap();
    assert_eq!(project.input_reference(input_id(9)), None);

    let resolver = SourceResolver::new(&project);
    let history = MixFeedHistory::<()>::new();
    let context = resolver
        .context(
            MixSelection {
                program: input_id(9),
                preview: input_id(1),
            },
            &history,
        )
        .unwrap();
    let title_source = context.resolve_input(input_id(9)).unwrap();
    assert_eq!(title_source.source, RenderSource::Input(input_id(9)));
    assert_eq!(title_source.alias_chain, Vec::<InputId>::new());
    assert_eq!(resolver.resolve_audio(input_id(9)), Ok(AudioRoute::Silence));
}

#[test]
fn title_inputs_refuse_out_of_bounds_and_misordered_fields() {
    use fm_model::{
        MAX_TITLE_CONTENT_BYTES, MAX_TITLE_FIELDS, MAX_TITLE_TEXT_CHARS, TitleFieldValue,
    };

    let mut blank_template = title_project(Vec::new());
    blank_template
        .replace_input_source(
            input_id(9),
            InputKind::Title(fm_model::TitleInput {
                template_uri: "   ".into(),
                field_revision: 0,
                fields: Vec::new(),
            }),
            Vec::new(),
        )
        .unwrap();
    assert_eq!(
        title_errors(&blank_template),
        [("kind.title.template_uri", ValidationErrorKind::Empty)]
    );

    let too_many = (1..=u128::try_from(MAX_TITLE_FIELDS).unwrap() + 1)
        .map(|id| title_field(id, TitleFieldValue::Number(0)))
        .collect();
    assert_eq!(
        title_errors(&title_project(too_many)),
        [("kind.title.fields", ValidationErrorKind::OutOfRange)]
    );
    let at_limit = (1..=u128::try_from(MAX_TITLE_FIELDS).unwrap())
        .map(|id| title_field(id, TitleFieldValue::Number(0)))
        .collect();
    title_project(at_limit).validate().unwrap();

    // Multibyte text counts characters, not bytes.
    title_project(vec![title_field(
        1,
        TitleFieldValue::Text("é".repeat(MAX_TITLE_TEXT_CHARS)),
    )])
    .validate()
    .unwrap();
    assert_eq!(
        title_errors(&title_project(vec![title_field(
            1,
            TitleFieldValue::Text("x".repeat(MAX_TITLE_TEXT_CHARS + 1)),
        )])),
        [("kind.title.fields.value", ValidationErrorKind::OutOfRange)]
    );

    let filler = "x".repeat(MAX_TITLE_TEXT_CHARS);
    let mut content = (1..=3)
        .map(|id| title_field(id, TitleFieldValue::Text(filler.clone())))
        .collect::<Vec<_>>();
    assert_eq!(3 * MAX_TITLE_TEXT_CHARS, MAX_TITLE_CONTENT_BYTES);
    title_project(content.clone()).validate().unwrap();
    content.push(title_field(
        4,
        TitleFieldValue::Image {
            asset_uri: "a".into(),
        },
    ));
    assert_eq!(
        title_errors(&title_project(content)),
        [("kind.title.fields", ValidationErrorKind::OutOfRange)]
    );

    assert_eq!(
        title_errors(&title_project(vec![title_field(
            1,
            TitleFieldValue::Image {
                asset_uri: " ".into()
            },
        )])),
        [("kind.title.fields.asset_uri", ValidationErrorKind::Empty)]
    );
    assert_eq!(
        title_errors(&title_project(vec![
            title_field(2, TitleFieldValue::Number(1)),
            title_field(1, TitleFieldValue::Number(2)),
        ])),
        [("kind.title.fields.id", ValidationErrorKind::OutOfOrder)]
    );
    assert_eq!(
        title_errors(&title_project(vec![
            title_field(1, TitleFieldValue::Number(1)),
            title_field(1, TitleFieldValue::Number(2)),
        ])),
        [("kind.title.fields.id", ValidationErrorKind::DuplicateId)]
    );
}

#[test]
fn set_title_fields_replaces_values_atomically() {
    use fm_model::{SetTitleFieldsError, TitleFieldValue, TitleFieldsError, TitleInput};

    let mut project = title_project(vec![title_field(1, TitleFieldValue::Number(1))]);
    let title = |project: &Project| match &project
        .inputs()
        .iter()
        .find(|input| input.id == input_id(9))
        .unwrap()
        .kind
    {
        InputKind::Title(title) => title.clone(),
        _ => unreachable!(),
    };

    project
        .set_title_fields(
            input_id(9),
            4,
            vec![
                title_field(1, TitleFieldValue::Number(7)),
                title_field(2, TitleFieldValue::Text("Live".into())),
            ],
        )
        .unwrap();
    assert_eq!(
        title(&project),
        TitleInput {
            template_uri: "asset://titles/lower-third.fmtitle".into(),
            field_revision: 4,
            fields: vec![
                title_field(1, TitleFieldValue::Number(7)),
                title_field(2, TitleFieldValue::Text("Live".into())),
            ],
        }
    );
    // The same revision may be rewritten; an older one may not.
    project
        .set_title_fields(
            input_id(9),
            4,
            vec![title_field(1, TitleFieldValue::Number(8))],
        )
        .unwrap();
    let before = project.clone();
    assert_eq!(
        project.set_title_fields(input_id(9), 3, Vec::new()),
        Err(SetTitleFieldsError::RevisionRegressed {
            current: 4,
            requested: 3,
        })
    );
    assert_eq!(
        project.set_title_fields(
            input_id(9),
            5,
            vec![
                title_field(2, TitleFieldValue::Number(1)),
                title_field(1, TitleFieldValue::Number(1)),
            ],
        ),
        Err(SetTitleFieldsError::InvalidFields(
            TitleFieldsError::OutOfOrder {
                previous: fm_model::TitleFieldId::new(NonZeroU128::new(2).unwrap()),
                next: fm_model::TitleFieldId::new(NonZeroU128::new(1).unwrap()),
            }
        ))
    );
    // A field may not be newer than its title (schema 78).
    let mut ahead = title_field(1, TitleFieldValue::Number(1));
    ahead.revision = 6;
    assert_eq!(
        project.set_title_fields(input_id(9), 5, vec![ahead]),
        Err(SetTitleFieldsError::InvalidFields(
            TitleFieldsError::RevisionAhead {
                field: fm_model::TitleFieldId::new(NonZeroU128::new(1).unwrap()),
                revision: 6,
                field_revision: 5,
            }
        ))
    );
    assert_eq!(
        project.set_title_fields(input_id(77), 5, Vec::new()),
        Err(SetTitleFieldsError::UnknownInput(input_id(77)))
    );
    assert_eq!(
        project.set_title_fields(input_id(1), 5, Vec::new()),
        Err(SetTitleFieldsError::NotTitleInput(input_id(1)))
    );
    assert_eq!(project, before);
}

/// A process-unique layer id, so layers built by helpers never collide.
fn next_layer_id() -> fm_model::LayerId {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);
    fm_model::LayerId::new(NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
        .expect("layer ids start at one")
}

/// A key/fill key input is a leaf picture (SW-008): replacing the source of
/// an input a key uses so that it renders a scene (an alias re-pointed at a
/// Scene input, directly or further down its chain, or the input itself
/// becoming a Scene input) is refused at edit time, leaving the project
/// valid and unchanged.
#[test]
fn source_replacement_refuses_a_key_input_that_would_render_a_scene() {
    let alias = |target: u128| InputKind::Alias {
        target: input_id(target),
        crop: None,
    };
    let mut project = Project::new(project_id(91), "Key source", settings());
    for (id, name, kind) in [
        (95, "Fill", InputKind::Color),
        (96, "Picture", InputKind::Color),
        (
            97,
            "Nested",
            InputKind::Scene {
                scene_id: scene_id(96),
                audio_source: None,
            },
        ),
        (98, "Key alias", alias(99)),
        (99, "Inner alias", alias(96)),
        (100, "Spare alias", alias(96)),
    ] {
        project.add_input(Input {
            id: input_id(id),
            name: name.into(),
            kind,
            required_capabilities: Vec::new(),
        });
    }
    for (id, name) in [(97, "Keyed"), (96, "Other")] {
        project.add_scene(Scene {
            id: scene_id(id),
            name: name.into(),
            background: Rgba8::OPAQUE_BLACK,
            layers: vec![layer("Fill", SourceRef::Input(input_id(95)))],
            animation: fm_model::SceneAnimation::default(),
        });
    }
    project
        .set_scene_layer_key(
            scene_id(97),
            0,
            Some(LayerKey::Fill(fm_model::FillLayerKey::linear(input_id(98)))),
        )
        .unwrap();
    assert_eq!(project.validate(), Ok(()));
    let before = project.clone();

    // The key's own alias, an alias further down its chain, and the key
    // input becoming a Scene input are all refused.
    for (input, kind) in [
        (98, alias(97)),
        (99, alias(97)),
        (
            98,
            InputKind::Scene {
                scene_id: scene_id(96),
                audio_source: None,
            },
        ),
    ] {
        assert_eq!(
            project.replace_input_source(input_id(input), kind, Vec::new()),
            Err(ReplaceInputError::SceneKeyInput(input_id(98)))
        );
        assert_eq!(project, before);
    }

    // An alias no key uses may still show a scene, and the key's alias may
    // still be re-pointed at another picture.
    project
        .replace_input_source(input_id(100), alias(97), Vec::new())
        .unwrap();
    project
        .replace_input_source(input_id(98), alias(95), Vec::new())
        .unwrap();
    assert_eq!(project.validate(), Ok(()));
}

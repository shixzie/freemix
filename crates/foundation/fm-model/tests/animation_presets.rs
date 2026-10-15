//! The project animation preset library (GX-011, schema 34): checked
//! insert, rename, and remove; project validation; and saving a scene
//! layer's phase and applying it to another layer.

use std::num::{NonZeroU32, NonZeroU128};

use fm_model::{
    AnimationPresetError, AnimationPresetId, AnimationPresetLibrary, EntityRef, Layer,
    LayerGeometry, LayerId, MAX_ANIMATION_PRESET_NAME_BYTES, MAX_PROJECT_ANIMATION_PRESETS,
    Project, ProjectSettings, Rotation, Scene, SceneAnimation, SceneAnimationError, SourceRef,
    UserAnimationPreset, ValidationErrorKind,
    fm_animation::{
        AnimationPreset, Clip, Easing, Keyframe, MotionTarget, Phase, PhaseMotion, Property, Rgba,
        Timebase, Track, TrackKey, Value,
    },
};
use fm_types::{
    AudioFormat, ChannelLayout, ColorMetadata, FrameRate, InputId, PixelFormat, ProjectId,
    SampleFormat, SampleRate, ScanMode, SceneId, VideoDimensions, VideoFormat,
};

fn preset_id(value: u32) -> AnimationPresetId {
    AnimationPresetId::new(NonZeroU32::new(value).unwrap())
}

fn layer_id(value: u32) -> LayerId {
    LayerId::new(value).unwrap()
}

fn timebase() -> Timebase {
    Timebase::new(FrameRate::new(30, 1).unwrap())
}

fn fade() -> PhaseMotion {
    let track = Track::new(
        TrackKey::new(MotionTarget, Phase::In, Property::Opacity),
        vec![
            Keyframe::new(0, Value::Scalar(0), Easing::Linear),
            Keyframe::new(12, Value::Scalar(255), Easing::Linear),
        ],
    );
    PhaseMotion::new(
        Clip::from_tracks(timebase(), 12, 0, vec![track]).unwrap(),
        100,
        50,
    )
    .unwrap()
}

fn preset(id: u32, name: &str) -> UserAnimationPreset {
    UserAnimationPreset {
        id: preset_id(id),
        name: name.into(),
        phase: Phase::In,
        motion: fade(),
    }
}

fn layer(id: u32, width: u32, height: u32) -> Layer {
    Layer {
        id: layer_id(id),
        name: format!("Layer {id}"),
        source: SourceRef::Input(InputId::new(NonZeroU128::new(1).unwrap())),
        enabled: true,
        geometry: LayerGeometry::new(10, 20, width, height, Rotation::Deg0),
        crop: None,
        mask: None,
        key: None,
        effects: Vec::new(),
        opacity: 255,
        z_order: 0,
    }
}

fn scene() -> Scene {
    Scene {
        id: SceneId::new(NonZeroU128::new(1).unwrap()),
        name: "Scene".into(),
        background: fm_model::Rgba8::OPAQUE_BLACK,
        layers: vec![layer(1, 400, 100), layer(2, 800, 200)],
        animation: SceneAnimation::default(),
    }
}

fn project() -> Project {
    let frame_rate = FrameRate::new(30, 1).unwrap();
    Project::new(
        ProjectId::new(NonZeroU128::new(1).unwrap()),
        "Presets",
        ProjectSettings {
            frame_rate,
            video: VideoFormat {
                dimensions: VideoDimensions::new(64, 36).unwrap(),
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
        },
    )
}

#[test]
fn library_edits_are_checked() {
    let mut library = AnimationPresetLibrary::default();
    library.insert(None, preset(1, "Fade up")).unwrap();
    library.insert(Some(0), preset(2, "Swoosh")).unwrap();
    assert_eq!(
        library
            .presets()
            .iter()
            .map(|preset| preset.id.get().get())
            .collect::<Vec<_>>(),
        [2, 1]
    );
    assert_eq!(
        library.insert(None, preset(2, "Other")),
        Err(AnimationPresetError::DuplicateId(preset_id(2)))
    );
    assert_eq!(
        library.insert(None, preset(3, "Swoosh")),
        Err(AnimationPresetError::DuplicateName("Swoosh".into()))
    );
    assert_eq!(
        library.insert(None, preset(3, "  ")),
        Err(AnimationPresetError::EmptyName)
    );
    assert_eq!(
        library.insert(None, preset(3, "a\tb")),
        Err(AnimationPresetError::NameControlCharacter)
    );
    let long = "x".repeat(MAX_ANIMATION_PRESET_NAME_BYTES + 1);
    assert_eq!(
        library.insert(None, preset(3, &long)),
        Err(AnimationPresetError::NameTooLong {
            bytes: MAX_ANIMATION_PRESET_NAME_BYTES + 1,
            maximum: MAX_ANIMATION_PRESET_NAME_BYTES,
        })
    );
    assert_eq!(
        library.insert(Some(3), preset(3, "Late")),
        Err(AnimationPresetError::IndexOutOfRange { index: 3, len: 2 })
    );
    assert_eq!(library.next_id(), Some(preset_id(3)));

    library.rename(preset_id(1), "Fade in".into()).unwrap();
    assert_eq!(library.get(preset_id(1)).unwrap().name, "Fade in");
    assert_eq!(
        library.rename(preset_id(1), "Swoosh".into()),
        Err(AnimationPresetError::DuplicateName("Swoosh".into()))
    );
    // Renaming to its own name is fine.
    library.rename(preset_id(2), "Swoosh".into()).unwrap();
    assert_eq!(
        library.rename(preset_id(9), "Nine".into()),
        Err(AnimationPresetError::UnknownPreset(preset_id(9)))
    );
    let (index, removed) = library.remove(preset_id(2)).unwrap();
    assert_eq!((index, removed.name.as_str()), (0, "Swoosh"));
    assert_eq!(
        library.remove(preset_id(2)),
        Err(AnimationPresetError::UnknownPreset(preset_id(2)))
    );
}

#[test]
fn library_is_bounded() {
    let mut library = AnimationPresetLibrary::default();
    for id in 1..=u32::try_from(MAX_PROJECT_ANIMATION_PRESETS).unwrap() {
        library
            .insert(None, preset(id, &format!("Preset {id}")))
            .unwrap();
    }
    assert_eq!(
        library.insert(None, preset(1_000, "One more")),
        Err(AnimationPresetError::TooManyPresets {
            maximum: MAX_PROJECT_ANIMATION_PRESETS
        })
    );
}

#[test]
fn project_validation_reports_library_violations() {
    let mut project = project();
    project.set_animation_presets(AnimationPresetLibrary::checked(vec![preset(1, "A")]).unwrap());
    assert!(project.validate().is_ok());
    project.set_animation_presets(AnimationPresetLibrary::from_presets_unchecked(vec![
        preset(1, "A"),
        preset(1, "A"),
        preset(2, ""),
    ]));
    let errors = project.validate().unwrap_err();
    let reported = |field: &str, kind: ValidationErrorKind, id: u32| {
        errors.iter().any(|error| {
            error.field == field
                && error.kind == kind
                && error.entity == Some(EntityRef::AnimationPreset(preset_id(id)))
        })
    };
    assert!(reported(
        "animation_presets.id",
        ValidationErrorKind::DuplicateId,
        1
    ));
    assert!(reported(
        "animation_presets.name",
        ValidationErrorKind::DuplicateName,
        1
    ));
    assert!(reported(
        "animation_presets.name",
        ValidationErrorKind::Empty,
        2
    ));
}

#[test]
fn a_saved_layer_phase_applies_to_another_layer() {
    let mut scene = scene();
    scene
        .apply_animation_preset(layer_id(1), Phase::In, AnimationPreset::Wipe, 20, 64, 36)
        .unwrap();
    let motion = scene
        .capture_animation_motion(layer_id(1), Phase::In)
        .unwrap();
    assert_eq!(
        (motion.width(), motion.height(), motion.frames()),
        (400, 100, 20)
    );
    assert_eq!(
        scene.capture_animation_motion(layer_id(2), Phase::In),
        Err(fm_model::fm_animation::MotionError::Empty)
    );
    let revision = scene
        .apply_animation_motion(layer_id(2), Phase::Out, &motion, 10)
        .unwrap();
    assert_eq!(revision, 2);
    let clip = &scene.animation.clip;
    assert_eq!(clip.out_frames(), 10);
    let track = clip
        .track(TrackKey::new(layer_id(2), Phase::Out, Property::CropRight))
        .unwrap();
    // The 400 px wipe covers the 800 px layer, over 10 frames.
    assert_eq!(
        track
            .keys
            .iter()
            .map(|key| (key.frame, key.value))
            .collect::<Vec<_>>(),
        [(0, Value::Scalar(800)), (10, Value::Scalar(0))]
    );
    assert_eq!(
        scene.apply_animation_motion(layer_id(9), Phase::In, &motion, 10),
        Err(SceneAnimationError::UnknownLayer(layer_id(9)))
    );
}

#[test]
fn a_colour_only_motion_does_not_apply_to_a_scene_layer() {
    let colour = Track::new(
        TrackKey::new(MotionTarget, Phase::In, Property::Color),
        vec![Keyframe::new(
            0,
            Value::Color(Rgba::new(1, 2, 3, 4)),
            Easing::Linear,
        )],
    );
    let motion = PhaseMotion::new(
        Clip::from_tracks(timebase(), 5, 0, vec![colour]).unwrap(),
        10,
        10,
    )
    .unwrap();
    let mut scene = scene();
    let before = scene.clone();
    assert_eq!(
        scene.apply_animation_motion(layer_id(1), Phase::In, &motion, 5),
        Err(SceneAnimationError::UnsupportedProperty {
            layer: layer_id(1),
            property: Property::Color,
        })
    );
    assert_eq!(scene, before);
}

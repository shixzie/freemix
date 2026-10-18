//! Scene layer animations at schema 32 (GX-011 stage 5): stable layer ids and
//! each scene's `animation` round-trip exactly through `project.json`, and
//! the codec is strict.

use std::{
    fs,
    num::NonZeroU128,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use fm_model::{
    Input, InputKind, Layer, LayerGeometry, LayerId, Project, ProjectSettings, Rgba8, Rotation,
    Scene, SceneAnimation, SimulatedAudio, SimulatedInput, SimulatedVideo, SourceRef,
    fm_animation::{
        AnimationPreset, Clip, CubicBezier, Easing, Keyframe, Phase, Property, Track, TrackKey,
        Value,
    },
};
use fm_persistence::{
    ProjectPosition, ProjectStore, ProjectValidationError, RuntimeRouting, StoreError,
    StoredProject,
};
use fm_types::{
    AudioFormat, ChannelLayout, ColorMetadata, FrameRate, InputId, PixelFormat, ProjectId,
    SampleFormat, SampleRate, ScanMode, SceneId, VideoDimensions, VideoFormat,
};

static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(name: &str) -> Self {
        let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "fm-persistence-scene-animation-{}-{sequence}-{name}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }

    fn store(&self, name: &str) -> ProjectStore {
        ProjectStore::new(self.0.join(name)).unwrap()
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn input_id(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn scene_id(value: u128) -> SceneId {
    SceneId::new(NonZeroU128::new(value).unwrap())
}

fn layer_id(value: u32) -> LayerId {
    LayerId::new(value).unwrap()
}

fn layer(id: u32, name: &str, x: i32) -> Layer {
    Layer {
        id: layer_id(id),
        name: name.into(),
        source: SourceRef::Input(input_id(1)),
        enabled: true,
        geometry: LayerGeometry::new(x, 0, 8, 9, Rotation::Deg0),
        crop: None,
        mask: None,
        key: None,
        effects: Vec::new(),
        opacity: u8::MAX,
        z_order: 0,
    }
}

fn project() -> Project {
    let frame_rate = FrameRate::new(30, 1).unwrap();
    let mut project = Project::new(
        ProjectId::new(NonZeroU128::new(7).unwrap()),
        "Animated",
        ProjectSettings {
            frame_rate,
            video: VideoFormat {
                dimensions: VideoDimensions::new(16, 9).unwrap(),
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
    );
    project.add_input(Input {
        id: input_id(1),
        name: "Bars".into(),
        kind: InputKind::Simulated(SimulatedInput::new(
            SimulatedVideo::Bars,
            SimulatedAudio::Silence,
        )),
        required_capabilities: Vec::new(),
    });
    let mut scene = Scene {
        id: scene_id(1),
        name: "Two up".into(),
        background: Rgba8::OPAQUE_BLACK,
        layers: vec![layer(4, "Left", 0), layer(9, "Right", 8)],
        animation: SceneAnimation::default(),
    };
    scene
        .apply_animation_preset(
            layer_id(9),
            Phase::In,
            AnimationPreset::SlideFromRight,
            12,
            16,
            9,
        )
        .unwrap();
    scene
        .edit_animation(|clip| {
            clip.set_phase_frames_keeping_keys(Phase::Out, 6)?;
            clip.set_track(Track::new(
                TrackKey::new(layer_id(4), Phase::Out, Property::Opacity),
                vec![
                    Keyframe::new(0, Value::Scalar(255), Easing::Step),
                    Keyframe::new(
                        6,
                        Value::Scalar(0),
                        Easing::CubicBezier(CubicBezier::new(1, -2, 3, 70_000).unwrap()),
                    ),
                ],
            ))
        })
        .unwrap();
    project.add_scene(scene);
    project.add_scene(Scene {
        id: scene_id(2),
        name: "Still".into(),
        background: Rgba8::OPAQUE_BLACK,
        layers: Vec::new(),
        animation: SceneAnimation::default(),
    });
    project.add_input(Input {
        id: input_id(2),
        name: "Two up".into(),
        kind: InputKind::Scene {
            scene_id: scene_id(1),
            audio_source: None,
        },
        required_capabilities: Vec::new(),
    });
    project
}

fn stored(project: Project) -> StoredProject {
    StoredProject::from_project(
        project,
        RuntimeRouting::default(),
        ProjectPosition::default(),
        Vec::new(),
    )
    .unwrap()
}

#[test]
fn scene_animations_and_layer_ids_round_trip_exactly() {
    let temp = TestDirectory::new("round-trip");
    let store = temp.store("show.freemix");
    let expected = stored(project());
    store.save(&expected).unwrap();
    let encoded = fs::read_to_string(store.manifest_path()).unwrap();
    assert!(encoded.starts_with("{\n  \"schema_version\": 51,\n"));
    assert!(
        encoded.contains("{\n            \"id\": 9,\n            \"name\": \"Right\""),
        "{encoded}"
    );
    assert!(
        encoded.contains(
            "\n        \"animation\": {\"timebase\": \"30/1\", \"revision\": 2, \"in_frames\": 12, \
             \"out_frames\": 6, \"tracks\": [\n          {\"layer\": 4, \"phase\": \"out\", \
             \"property\": \"opacity\", \"keys\": [{\"frame\": 0, \"value\": 255, \"easing\": \
             \"step\"}, {\"frame\": 6, \"value\": 0, \"easing\": {\"cubic_bezier\": [1, -2, 3, \
             70000]}}]},\n          {\"layer\": 9, \"phase\": \"in\", \"property\": \
             \"position_x\", \"keys\": [{\"frame\": 0, \"value\": 8, \"easing\": \"cubic-out\"}, \
             {\"frame\": 12, \"value\": 0, \"easing\": \"linear\"}]}\n        ]}\n      }"
        ),
        "{encoded}"
    );
    assert!(
        encoded.contains(
            "\"layers\": [],\n        \"animation\": {\"timebase\": \"30/1\", \"revision\": 0, \
             \"in_frames\": 0, \"out_frames\": 0, \"tracks\": []}\n      }"
        ),
        "{encoded}"
    );
    let loaded = store.load().unwrap();
    assert_eq!(loaded, expected);
    assert_eq!(fm_persistence_reencode(&store, &loaded), encoded);
}

fn fm_persistence_reencode(store: &ProjectStore, loaded: &StoredProject) -> String {
    store.save(loaded).unwrap();
    fs::read_to_string(store.manifest_path()).unwrap()
}

#[test]
fn scene_animation_documents_are_strict() {
    let temp = TestDirectory::new("strict");
    let store = temp.store("show.freemix");
    store.save(&stored(project())).unwrap();
    let encoded = fs::read_to_string(store.manifest_path()).unwrap();

    // Shape errors: missing, unknown, or mistyped keys and values.
    for malformed in [
        encoded.replacen("\n            \"id\": 9,", "", 1),
        encoded.replacen(
            "\"id\": 9,\n            \"name\"",
            "\"id\": 0,\n            \"name\"",
            1,
        ),
        encoded.replacen(
            ",\n        \"animation\": {\"timebase\": \"30/1\", \"revision\": 0, \"in_frames\": 0, \
             \"out_frames\": 0, \"tracks\": []}",
            "",
            1,
        ),
        encoded.replacen("\"timebase\": \"30/1\"", "\"timebase\": \"60/2\"", 1),
        encoded.replacen("\"property\": \"opacity\"", "\"property\": \"color\"", 1),
        encoded.replacen("\"phase\": \"out\"", "\"phase\": \"hold\"", 1),
        encoded.replacen("\"easing\": \"step\"", "\"easing\": \"stepped\"", 1),
        encoded.replacen("{\"layer\": 4,", "{\"layer\": 0,", 1),
        // Tracks out of clip order.
        encoded.replacen("{\"layer\": 4,", "{\"layer\": 10,", 1),
        encoded.replacen("\"revision\": 2,", "\"revision\": 2, \"extra\": 1,", 1),
        // Keys after the phase end, and non-increasing keys.
        encoded.replacen(
            "{\"frame\": 6, \"value\": 0",
            "{\"frame\": 7, \"value\": 0",
            1,
        ),
        encoded.replacen(
            "{\"frame\": 6, \"value\": 0",
            "{\"frame\": 0, \"value\": 0",
            1,
        ),
    ] {
        assert_ne!(malformed, encoded);
        fs::write(store.manifest_path(), &malformed).unwrap();
        let result = store.load();
        assert!(
            matches!(result, Err(StoreError::MalformedManifest { .. })),
            "accepted or misreported malformed animation: {result:?}"
        );
    }

    // Domain errors: a track naming a layer the scene does not have, and
    // duplicate layer ids.
    for invalid in [
        encoded.replacen("{\"layer\": 9,", "{\"layer\": 10,", 1),
        encoded.replacen(
            "\"id\": 9,\n            \"name\"",
            "\"id\": 4,\n            \"name\"",
            1,
        ),
    ] {
        assert_ne!(invalid, encoded);
        fs::write(store.manifest_path(), &invalid).unwrap();
        let result = store.load();
        assert!(
            matches!(
                result,
                Err(StoreError::Validation(
                    ProjectValidationError::DomainProject(_)
                ))
            ),
            "accepted or misreported invalid animation: {result:?}"
        );
    }
}

#[test]
fn layer_edits_keep_ids_and_removed_layers_take_their_tracks() {
    let mut project = project();
    // A duplicate takes the next free id; the original keeps its own.
    project
        .duplicate_scene_layer(scene_id(1), 1, "Right copy".into())
        .unwrap();
    let ids = project.scenes()[0]
        .layers
        .iter()
        .map(|layer| layer.id.get())
        .collect::<Vec<_>>();
    assert_eq!(ids, [4, 9, 10]);
    // Removing the animated layer drops its tracks and advances the revision.
    let before = project.scenes()[0].animation.revision;
    project.remove_layer_from_scene(scene_id(1), 1).unwrap();
    let animation = &project.scenes()[0].animation;
    assert_eq!(animation.revision, before + 1);
    assert!(
        animation
            .clip
            .tracks()
            .iter()
            .all(|track| track.key.target == layer_id(4))
    );
    assert_eq!(project.validate(), Ok(()));
    // A whole clip replacing the animation is checked against the layers.
    let foreign = Clip::from_tracks(
        fm_model::default_scene_timebase(),
        5,
        0,
        vec![Track::new(
            TrackKey::new(layer_id(9), Phase::In, Property::PositionX),
            vec![Keyframe::new(0, Value::Scalar(1), Easing::Linear)],
        )],
    )
    .unwrap();
    assert!(
        project
            .set_scene_animation(scene_id(1), SceneAnimation::new(foreign, 9))
            .is_err()
    );
}

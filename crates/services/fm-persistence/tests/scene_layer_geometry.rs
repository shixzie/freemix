//! Schema 38 (SW-006): a scene layer's free rotation (quarter turns plus a
//! stored tilt in millidegrees) and its anchor round-trip exactly through
//! `project.json`, and the codec is strict about them.

use std::{
    fs,
    num::NonZeroU128,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use fm_model::{
    CURRENT_SCHEMA_VERSION, Input, InputKind, Layer, LayerAnchor, LayerGeometry, LayerId, Project,
    ProjectSettings, Rgba8, Rotation, Scene, SceneAnimation, SimulatedAudio, SimulatedInput,
    SimulatedVideo, SourceRef,
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
            "fm-persistence-scene-layer-geometry-{}-{sequence}-{name}",
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

fn layer(id: u32, geometry: LayerGeometry) -> Layer {
    Layer {
        id: LayerId::new(id).unwrap(),
        name: format!("Layer {id}"),
        source: SourceRef::Input(input_id(1)),
        enabled: true,
        geometry,
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
        "Turned",
        ProjectSettings {
            frame_rate,
            video: VideoFormat {
                dimensions: VideoDimensions::new(1_920, 1_080).unwrap(),
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
    project.add_scene(Scene {
        id: SceneId::new(NonZeroU128::new(1).unwrap()),
        name: "Turned".into(),
        background: Rgba8::OPAQUE_BLACK,
        layers: vec![
            layer(1, LayerGeometry::new(0, 0, 1_920, 1_080, Rotation::Deg0)),
            layer(
                2,
                LayerGeometry::new(-40, 30, 640, 360, Rotation::Deg270)
                    .with_tilt(-12_345)
                    .with_anchor(LayerAnchor::new(0, 10_000)),
            ),
        ],
        animation: SceneAnimation::default(),
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

const TURNED: &str = "\"geometry\": {\"translation_x\": -40, \"translation_y\": 30, \"width\": \
                      640, \"height\": 360, \"rotation\": \"deg270\", \"tilt_millidegrees\": \
                      -12345, \"anchor_x\": 0, \"anchor_y\": 10000}";

#[test]
fn free_rotation_and_anchor_round_trip_exactly() {
    assert_eq!(CURRENT_SCHEMA_VERSION.get(), 99);
    let temp = TestDirectory::new("round-trip");
    let store = temp.store("show.freemix");
    let expected = stored(project());
    store.save(&expected).unwrap();
    let encoded = fs::read_to_string(store.manifest_path()).unwrap();
    assert!(encoded.starts_with("{\n  \"schema_version\": 99,\n"));
    assert!(encoded.contains(TURNED), "{encoded}");
    // An unturned layer anchored at its centre spells out the defaults.
    assert!(
        encoded.contains(
            "\"geometry\": {\"translation_x\": 0, \"translation_y\": 0, \"width\": 1920, \
             \"height\": 1080, \"rotation\": \"deg0\", \"tilt_millidegrees\": 0, \"anchor_x\": \
             5000, \"anchor_y\": 5000}"
        ),
        "{encoded}"
    );
    let loaded = store.load().unwrap();
    assert_eq!(loaded, expected);
    store.save(&loaded).unwrap();
    assert_eq!(fs::read_to_string(store.manifest_path()).unwrap(), encoded);
}

#[test]
fn layer_geometry_documents_are_strict() {
    let temp = TestDirectory::new("strict");
    let store = temp.store("show.freemix");
    store.save(&stored(project())).unwrap();
    let encoded = fs::read_to_string(store.manifest_path()).unwrap();
    // Every field is required, typed, and alone.
    for malformed in [
        encoded.replacen(", \"tilt_millidegrees\": -12345", "", 1),
        encoded.replacen(", \"anchor_x\": 0", "", 1),
        encoded.replacen(", \"anchor_y\": 10000", "", 1),
        encoded.replacen(
            "\"tilt_millidegrees\": -12345",
            "\"tilt_millidegrees\": \"-12345\"",
            1,
        ),
        encoded.replacen("\"anchor_x\": 0", "\"anchor_x\": -1", 1),
        encoded.replacen("\"anchor_y\": 10000", "\"anchor_y\": 65536", 1),
        encoded.replacen(
            "\"anchor_y\": 10000}",
            "\"anchor_y\": 10000, \"pivot\": 0}",
            1,
        ),
    ] {
        assert_ne!(malformed, encoded);
        fs::write(store.manifest_path(), &malformed).unwrap();
        let result = store.load();
        assert!(
            matches!(result, Err(StoreError::MalformedManifest { .. })),
            "accepted or misreported malformed geometry: {result:?}"
        );
    }
    // A tilt of half a quarter turn or more, or an anchor outside the box,
    // is a domain error.
    for invalid in [
        encoded.replacen(
            "\"tilt_millidegrees\": -12345",
            "\"tilt_millidegrees\": 45000",
            1,
        ),
        encoded.replacen(
            "\"tilt_millidegrees\": -12345",
            "\"tilt_millidegrees\": -45001",
            1,
        ),
        encoded.replacen("\"anchor_y\": 10000", "\"anchor_y\": 10001", 1),
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
            "accepted or misreported invalid geometry: {result:?}"
        );
    }
}

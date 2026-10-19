//! OR-011 at schema 40: the project's fullscreen display-output slots
//! round-trip exactly and strictly in `project.json`.

use std::{
    fs,
    num::NonZeroU128,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use fm_model::{
    AudioBus, Input, InputKind, Output, Project, ProjectSettings, Rgba8, Scene, SimulatedAudio,
    SimulatedInput, SimulatedVideo, SolidColor, StartupPolicy,
};
use fm_persistence::{ProjectPosition, ProjectStore, RuntimeRouting, StoreError, StoredProject};
use fm_types::display::{
    DisplayId, DisplayMode, DisplayOutputConfig, DisplayOutputSlot, DisplayOutputSource,
    DisplaySelector, HdrRequest, ModeRequest,
};
use fm_types::{
    AudioFormat, BusId, ChannelLayout, ColorMetadata, FrameRate, InputId, OutputId, OutputScaling,
    PixelFormat, ProjectId, SampleFormat, SampleRate, ScanMode, SceneId, VideoDimensions,
    VideoFormat,
};

static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(name: &str) -> Self {
        let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "fm-persistence-displays-{}-{sequence}-{name}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn id(value: u128) -> NonZeroU128 {
    NonZeroU128::new(value).unwrap()
}

fn project() -> Project {
    let frame_rate = FrameRate::new(30, 1).unwrap();
    let mut project = Project::new(
        ProjectId::new(id(1)),
        "Mixer",
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
    );
    for (value, name) in [(10, "Host"), (11, "Guest")] {
        project.add_input(Input {
            id: InputId::new(id(value)),
            name: name.into(),
            kind: InputKind::Simulated(SimulatedInput::new(
                SimulatedVideo::Solid(SolidColor::new(1, 2, 3, 255)),
                SimulatedAudio::Sine { frequency_hz: 440 },
            )),
            required_capabilities: Vec::new(),
        });
    }
    project.add_scene(Scene {
        id: SceneId::new(id(20)),
        name: "Scene".into(),
        background: Rgba8::OPAQUE_BLACK,
        layers: Vec::new(),
        animation: fm_model::SceneAnimation::default(),
    });
    project.add_audio_bus(AudioBus {
        id: BusId::new(id(30)),
        name: "Master".into(),
        sends: Vec::new(),
    });
    for (value, name) in [(40, "Program"), (41, "Clean")] {
        project.add_output(Output {
            id: OutputId::new(id(value)),
            name: name.into(),
            video_source: SceneId::new(id(20)),
            audio_source: BusId::new(id(30)),
            startup: StartupPolicy::Stopped,
            required_capabilities: Vec::new(),
        });
    }
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
#[allow(clippy::too_many_lines)]
fn display_output_slots_round_trip_exactly_and_strictly() {
    let directory = TestDirectory::new("round-trip");
    let store = ProjectStore::new(directory.path("show.freemix")).unwrap();
    let mut expected = project();
    let slot = |value| DisplayOutputSlot::new(value).unwrap();
    expected
        .set_display_output(
            slot(1),
            DisplayOutputConfig {
                display: DisplaySelector::IdOrName {
                    id: DisplayId::new("cg:69733382").unwrap(),
                    name: "DELL \"U2720Q\"".into(),
                },
                source: DisplayOutputSource::Program,
                mode: ModeRequest::Current,
                scaling: OutputScaling::Fit,
                hdr: HdrRequest::Off,
            },
        )
        .unwrap();
    expected
        .set_display_output(
            slot(2),
            DisplayOutputConfig {
                display: DisplaySelector::Index(1),
                source: DisplayOutputSource::Output(OutputId::new(id(41))),
                mode: ModeRequest::Exact(DisplayMode::new(1920, 1080, 59_940)),
                scaling: OutputScaling::OneToOne,
                hdr: HdrRequest::Auto,
            },
        )
        .unwrap();
    store.save(&stored(expected.clone())).unwrap();
    let loaded = store.load().unwrap();
    assert_eq!(loaded.project(), &expected);

    let text = fs::read_to_string(directory.path("show.freemix/project.json")).unwrap();
    assert!(text.contains("\"schema_version\": 60"));
    let first = "{\"slot\": 1, \"display\": {\"id\": \"cg:69733382\", \"name\": \"DELL \\\"U2720Q\\\"\"}, \"source\": \"program\", \"mode\": \"current\", \"scaling\": \"fit\", \"hdr\": \"off\"}";
    let second = "{\"slot\": 2, \"display\": {\"index\": 1}, \"source\": \"output:41\", \"mode\": {\"width\": 1920, \"height\": 1080, \"refresh_millihertz\": 59940}, \"scaling\": \"1:1\", \"hdr\": \"auto\"}";
    assert!(
        text.contains(&format!(
            "\"display_outputs\": [\n      {first},\n      {second}\n    ],\n    \"live_lan\": "
        )),
        "{text}"
    );

    // A name alone, and an id alone, round-trip too.
    let mut named = expected.clone();
    named.clear_display_output(slot(2));
    named
        .set_display_output(
            slot(1),
            "display=name:Wall,source=preview,scaling=stretch"
                .parse()
                .unwrap(),
        )
        .unwrap();
    named
        .set_display_output(slot(2), "display=id:edid:7,scaling=fill".parse().unwrap())
        .unwrap();
    store.save(&stored(named.clone())).unwrap();
    assert_eq!(store.load().unwrap().project(), &named);
    store.save(&stored(expected)).unwrap();

    for (from, to) in [
        // Structure: slots, ordering, count, and field shapes.
        ("{\"slot\": 2,", "{\"slot\": 3,"),
        ("{\"slot\": 2,", "{\"slot\": 0,"),
        ("{\"slot\": 2,", "{\"slot\": 1,"),
        (second, &format!("{second},\n      {second}")),
        ("\"display\": {\"index\": 1}", "\"display\": {}"),
        (
            "\"display\": {\"index\": 1}",
            "\"display\": {\"index\": 1, \"name\": \"Wall\"}",
        ),
        (
            "\"display\": {\"index\": 1}",
            "\"display\": {\"index\": -1}",
        ),
        (
            "\"display\": {\"index\": 1}",
            "\"display\": {\"position\": 1}",
        ),
        ("\"source\": \"output:41\"", "\"source\": \"output:0\""),
        ("\"source\": \"output:41\"", "\"source\": \"multiview\""),
        ("\"mode\": \"current\"", "\"mode\": \"native\""),
        (
            "\"refresh_millihertz\": 59940}",
            "\"refresh_millihertz\": \"59940\"}",
        ),
        ("\"scaling\": \"1:1\"", "\"scaling\": \"zoom\""),
        ("\"hdr\": \"auto\"", "\"hdr\": \"on\""),
        (", \"hdr\": \"auto\"", ""),
        ("\"hdr\": \"auto\"}", "\"hdr\": \"auto\", \"cursor\": true}"),
        (
            ",\n    \"display_outputs\": [",
            ",\n    \"display_outputs_v2\": [",
        ),
    ] {
        let malformed = text.replacen(from, to, 1);
        assert_ne!(malformed, text, "{from}");
        fs::write(directory.path("show.freemix/project.json"), malformed).unwrap();
        assert!(
            matches!(store.load(), Err(StoreError::MalformedManifest { .. })),
            "{from} -> {to}: {:?}",
            store.load()
        );
    }

    // Output references and one display per slot are project validation.
    for (from, to) in [
        ("\"source\": \"output:41\"", "\"source\": \"output:99\""),
        (
            "\"display\": {\"index\": 1}",
            "\"display\": {\"id\": \"cg:69733382\"}",
        ),
    ] {
        let invalid = text.replacen(from, to, 1);
        assert_ne!(invalid, text, "{from}");
        fs::write(directory.path("show.freemix/project.json"), invalid).unwrap();
        assert!(
            matches!(store.load(), Err(StoreError::Validation(_))),
            "{from} -> {to}: {:?}",
            store.load()
        );
    }
}

/// OR-017 at schema 53: the `LiveLAN` configuration round-trips exactly,
/// its PIN stored in the bundle, and is strict about its shape. A refused
/// PIN is never echoed into the error.
#[test]
fn live_lan_round_trips_exactly_and_strictly() {
    use fm_types::live_lan::{LiveLanConfig, LiveLanPin, LiveLanSource};
    let directory = TestDirectory::new("live-lan");
    let store = ProjectStore::new(directory.path("show.freemix")).unwrap();
    let mut expected = project();
    expected
        .set_live_lan(LiveLanConfig {
            enabled: true,
            source: LiveLanSource::Output(OutputId::new(id(41))),
            max_viewers: 12,
            pin: Some(LiveLanPin::parse("024680").unwrap()),
        })
        .unwrap();
    store.save(&stored(expected.clone())).unwrap();
    assert_eq!(store.load().unwrap().project(), &expected);
    let text = fs::read_to_string(directory.path("show.freemix/project.json")).unwrap();
    let live_lan = "\"live_lan\": {\"enabled\": true, \"source\": \"output:41\", \"max_viewers\": 12, \"pin\": \"024680\"},\n    \"scripts\": [],\n    \"script_runs\": {\"last_run\": 0, \"runs\": [], \"faults\": []}\n  },";
    assert!(text.contains(live_lan), "{text}");

    let mut open = expected.clone();
    open.set_live_lan(LiveLanConfig::default()).unwrap();
    store.save(&stored(open.clone())).unwrap();
    assert_eq!(store.load().unwrap().project(), &open);
    store.save(&stored(expected)).unwrap();

    for (from, to) in [
        ("\"enabled\": true", "\"enabled\": 1"),
        (
            "\"source\": \"output:41\", \"max",
            "\"source\": \"preview\", \"max",
        ),
        ("\"max_viewers\": 12", "\"max_viewers\": 300"),
        ("\"pin\": \"024680\"", "\"pin\": \"24a8\""),
        ("\"pin\": \"024680\"", "\"pin\": \"123\""),
        ("\"pin\": \"024680\"", "\"pin\": 2468"),
        (", \"pin\": \"024680\"", ""),
        ("\"pin\": \"024680\"}", "\"pin\": \"024680\", \"port\": 80}"),
        ("\"live_lan\": {", "\"livelan\": {"),
    ] {
        let malformed = text.replacen(from, to, 1);
        assert_ne!(malformed, text, "{from}");
        fs::write(directory.path("show.freemix/project.json"), malformed).unwrap();
        let error = store.load().unwrap_err();
        assert!(
            matches!(error, StoreError::MalformedManifest { .. }),
            "{from} -> {to}: {error:?}"
        );
        assert!(!format!("{error:?} {error}").contains("24a8"), "{error}");
    }
    for (from, to) in [
        (
            "\"source\": \"output:41\", \"max",
            "\"source\": \"output:99\", \"max",
        ),
        ("\"max_viewers\": 12", "\"max_viewers\": 0"),
        ("\"max_viewers\": 12", "\"max_viewers\": 33"),
    ] {
        let invalid = text.replacen(from, to, 1);
        assert_ne!(invalid, text, "{from}");
        fs::write(directory.path("show.freemix/project.json"), invalid).unwrap();
        assert!(
            matches!(store.load(), Err(StoreError::Validation(_))),
            "{from} -> {to}: {:?}",
            store.load()
        );
    }
}

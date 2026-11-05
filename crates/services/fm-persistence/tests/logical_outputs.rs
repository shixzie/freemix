//! SW-012 at schema 107: logical-output routing round-trips exactly and
//! strictly in `project.json`.

use std::{
    fs,
    num::NonZeroU128,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use fm_model::{
    AudioBus, Input, InputKind, LogicalAudioMap, LogicalMultiViewCell, LogicalOutputConfig,
    LogicalOutputSlot, LogicalOutputSource, LogicalOverlayChannels, LogicalOverlayPolicy, Output,
    Project, ProjectSettings, Rgba8, Scene, SimulatedAudio, SimulatedInput, SimulatedVideo,
    SolidColor, StartupPolicy,
};
use fm_persistence::{ProjectPosition, ProjectStore, RuntimeRouting, StoredProject};
use fm_types::{
    AudioFormat, BusId, ChannelLayout, ColorMetadata, FrameRate, InputId, OutputId, PixelFormat,
    ProjectId, SampleFormat, SampleRate, ScanMode, SceneId, VideoDimensions, VideoFormat,
};

static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(name: &str) -> Self {
        let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "fm-persistence-logical-outputs-{}-{sequence}-{name}",
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
        "Outputs",
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
    for (value, name) in [(30, "Master"), (31, "Aux")] {
        project.add_audio_bus(AudioBus {
            id: BusId::new(id(value)),
            name: name.into(),
            sends: Vec::new(),
        });
    }
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

fn slot(value: u8) -> LogicalOutputSlot {
    LogicalOutputSlot::new(value).unwrap()
}

fn configured() -> Project {
    let mut project = project();
    project
        .set_logical_output(
            slot(1),
            Some(LogicalOutputConfig {
                slot: slot(1),
                output: OutputId::new(id(40)),
                source: LogicalOutputSource::Program,
                overlays: LogicalOverlayPolicy::All,
                audio: LogicalAudioMap::Master,
            }),
        )
        .unwrap();
    project
        .set_logical_output(
            slot(2),
            Some(LogicalOutputConfig {
                slot: slot(2),
                output: OutputId::new(id(41)),
                source: LogicalOutputSource::Input(InputId::new(id(10))),
                overlays: LogicalOverlayPolicy::Channels(
                    LogicalOverlayChannels::from_channels([1, 3]).unwrap(),
                ),
                audio: LogicalAudioMap::Bus(BusId::new(id(31))),
            }),
        )
        .unwrap();
    project
        .set_logical_output(
            slot(3),
            Some(LogicalOutputConfig {
                slot: slot(3),
                output: OutputId::new(id(42)),
                source: LogicalOutputSource::MultiView(vec![
                    LogicalMultiViewCell::Program,
                    LogicalMultiViewCell::Input(InputId::new(id(11))),
                    LogicalMultiViewCell::Mix(2),
                    LogicalMultiViewCell::Output(slot(1)),
                ]),
                overlays: LogicalOverlayPolicy::Routed,
                audio: LogicalAudioMap::FollowVideo,
            }),
        )
        .unwrap_err();
    // Slot 3 needs its own project output first.
    project.add_output(Output {
        id: OutputId::new(id(42)),
        name: "Multiview".into(),
        video_source: SceneId::new(id(20)),
        audio_source: BusId::new(id(30)),
        startup: StartupPolicy::Stopped,
        required_capabilities: Vec::new(),
    });
    project
        .set_logical_output(
            slot(3),
            Some(LogicalOutputConfig {
                slot: slot(3),
                output: OutputId::new(id(42)),
                source: LogicalOutputSource::MultiView(vec![
                    LogicalMultiViewCell::Program,
                    LogicalMultiViewCell::Input(InputId::new(id(11))),
                    LogicalMultiViewCell::Mix(2),
                    LogicalMultiViewCell::Output(slot(1)),
                ]),
                overlays: LogicalOverlayPolicy::Routed,
                audio: LogicalAudioMap::FollowVideo,
            }),
        )
        .unwrap();
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
fn logical_outputs_round_trip_exactly_and_strictly() {
    let directory = TestDirectory::new("round-trip");
    let store = ProjectStore::new(directory.path("show.freemix")).unwrap();
    let expected = configured();
    store.save(&stored(expected.clone())).unwrap();
    let loaded = store.load().unwrap();
    assert_eq!(loaded.project(), &expected);

    let text = fs::read_to_string(directory.path("show.freemix/project.json")).unwrap();
    assert!(text.contains("\"schema_version\": 108"), "{text}");
    let first = "{\"slot\": 1, \"output\": 40, \"source\": \"program\", \"overlays\": \"all\", \"audio\": \"master\"}";
    let second = "{\"slot\": 2, \"output\": 41, \"source\": {\"input\": 10}, \"overlays\": {\"channels\": [1, 3]}, \"audio\": {\"bus\": 31}}";
    let third = "{\"slot\": 3, \"output\": 42, \"source\": {\"multiview\": [\"program\", {\"input\": 11}, {\"mix\": 2}, {\"output\": 1}]}, \"overlays\": \"routed\", \"audio\": \"follow_video\"}";
    assert!(
        text.contains(&format!(
            "\"logical_outputs\": [\n      {first},\n      {second},\n      {third}\n    ],\n    \"display_outputs\": "
        )),
        "{text}"
    );

    for (from, to) in [
        ("\"slot\": 2,", "\"slot\": 4,"),
        ("\"slot\": 2,", "\"slot\": 0,"),
        ("\"slot\": 2,", "\"slot\": 1,"),
        ("\"output\": 41", "\"output\": 99"),
        ("\"output\": 41", "\"output\": 40"),
        ("\"input\": 10", "\"input\": 99"),
        ("\"bus\": 31", "\"bus\": 99"),
        ("\"channels\": [1, 3]", "\"channels\": [1, 9]"),
        ("\"channels\": [1, 3]", "\"channels\": [1, 3, 3]"),
        ("\"mix\": 2", "\"mix\": 0"),
        ("\"mix\": 2", "\"mix\": 17"),
        ("{\"output\": 1}", "{\"output\": 3}"),
        ("{\"output\": 1}", "{\"output\": 4}"),
        (
            "\"multiview\": [\"program\",",
            "\"multiview\": [\"program\", \"program\",",
        ),
        (
            "[\"program\", {\"input\": 11}, {\"mix\": 2}, {\"output\": 1}]",
            "[]",
        ),
        ("\"source\": \"program\"", "\"source\": \"mix\""),
        ("\"overlays\": \"all\"", "\"overlays\": \"clean \""),
        ("\"audio\": \"master\"", "\"audio\": \"follow\""),
        ("\"slot\": 2,", "\"slot\": 2, \"extra\": 1,"),
    ] {
        assert!(text.contains(from), "{from}");
        let broken = text.replacen(from, to, 1);
        fs::write(directory.path("show.freemix/project.json"), broken).unwrap();
        assert!(
            store.load().is_err(),
            "accepted {from:?} replaced by {to:?}"
        );
    }
}

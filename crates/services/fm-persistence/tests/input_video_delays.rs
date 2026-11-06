//! IN-022 at schema 110: per-input video delays round-trip exactly and
//! strictly in `project.json`.

use std::{
    fs,
    num::NonZeroU128,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use fm_model::{
    Input, InputKind, MAX_INPUT_VIDEO_DELAY_FRAMES, Project, ProjectSettings, Rgba8, Scene,
    SimulatedAudio, SimulatedInput, SimulatedVideo, SolidColor,
};
use fm_persistence::{ProjectPosition, ProjectStore, RuntimeRouting, StoredProject};
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
            "fm-persistence-video-delays-{}-{sequence}-{name}",
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

fn input(value: u128) -> InputId {
    InputId::new(id(value))
}

fn project() -> Project {
    let frame_rate = FrameRate::new(30, 1).unwrap();
    let mut project = Project::new(
        ProjectId::new(id(1)),
        "Delays",
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
            id: input(value),
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
    project
}

fn configured() -> Project {
    let mut project = project();
    project.set_input_video_delay(input(10), 5).unwrap();
    project
        .set_input_video_delay(input(11), MAX_INPUT_VIDEO_DELAY_FRAMES)
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
fn input_video_delays_round_trip_exactly_and_strictly() {
    let directory = TestDirectory::new("round-trip");
    let store = ProjectStore::new(directory.path("show.freemix")).unwrap();
    let expected = configured();
    store.save(&stored(expected.clone())).unwrap();
    let loaded = store.load().unwrap();
    assert_eq!(loaded.project(), &expected);
    assert_eq!(loaded.project().input_video_delay(input(10)), 5);
    assert_eq!(
        loaded.project().input_video_delay(input(11)),
        MAX_INPUT_VIDEO_DELAY_FRAMES
    );

    let text = fs::read_to_string(directory.path("show.freemix/project.json")).unwrap();
    assert!(text.contains("\"schema_version\": 112"), "{text}");
    assert!(
        text.contains(
            "\"input_video_delays\": [\n      {\"input\": 10, \"frames\": 5},\n      {\"input\": 11, \"frames\": 600}\n    ]"
        ),
        "{text}"
    );

    for (from, to) in [
        ("\"input\": 10", "\"input\": 0"),
        ("\"input\": 10", "\"input\": 99"),
        ("\"input\": 10", "\"input\": 11"),
        ("\"frames\": 5", "\"frames\": 601"),
        ("\"frames\": 5", "\"frames\": 5.5"),
        ("\"input\": 10,", "\"input\": 10, \"extra\": 1,"),
        ("\"frames\": 5}", "\"frames\": 5, \"extra\": 1}"),
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

#[test]
fn setting_a_delay_is_bounded_and_zero_clears_it() {
    let mut project = project();
    project.set_input_video_delay(input(10), 12).unwrap();
    assert_eq!(project.input_video_delay(input(10)), 12);
    project.set_input_video_delay(input(10), 0).unwrap();
    assert_eq!(project.input_video_delay(input(10)), 0);
    assert!(project.input_video_delays().is_empty());

    assert!(project.set_input_video_delay(input(99), 1).is_err());
    assert!(
        project
            .set_input_video_delay(input(10), MAX_INPUT_VIDEO_DELAY_FRAMES + 1)
            .is_err()
    );
    assert!(project.input_video_delays().is_empty());
}

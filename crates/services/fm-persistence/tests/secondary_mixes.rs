//! IN-023 at schema 108: secondary mixes round-trip exactly and strictly in
//! `project.json`.

use std::{
    fs,
    num::NonZeroU128,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use fm_model::{
    Input, InputKind, MixNumber, Project, ProjectSettings, Rgba8, Scene, SecondaryMixConfig,
    SecondaryMixSource, SimulatedAudio, SimulatedInput, SimulatedVideo, SolidColor,
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
            "fm-persistence-secondary-mixes-{}-{sequence}-{name}",
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

fn number(value: u8) -> MixNumber {
    MixNumber::new(value).unwrap()
}

fn project() -> Project {
    let frame_rate = FrameRate::new(30, 1).unwrap();
    let mut project = Project::new(
        ProjectId::new(id(1)),
        "Mixes",
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
    project
        .set_secondary_mix(SecondaryMixConfig::preview_program(
            number(2),
            "Clean",
            SecondaryMixSource::Input(input(10)),
            SecondaryMixSource::Input(input(11)),
        ))
        .unwrap();
    project
        .set_secondary_mix(SecondaryMixConfig::output_only(
            number(3),
            "Feed",
            SecondaryMixSource::Mix(number(2)),
        ))
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
fn secondary_mixes_round_trip_exactly_and_strictly() {
    let directory = TestDirectory::new("round-trip");
    let store = ProjectStore::new(directory.path("show.freemix")).unwrap();
    let expected = configured();
    store.save(&stored(expected.clone())).unwrap();
    let loaded = store.load().unwrap();
    assert_eq!(loaded.project(), &expected);

    let text = fs::read_to_string(directory.path("show.freemix/project.json")).unwrap();
    assert!(text.contains("\"schema_version\": 108"), "{text}");
    let first = "{\"number\": 2, \"name\": \"Clean\", \"mode\": \"preview_program\", \"program\": {\"input\": 10}, \"preview\": {\"input\": 11}}";
    let second =
        "{\"number\": 3, \"name\": \"Feed\", \"mode\": \"output_only\", \"program\": {\"mix\": 2}}";
    assert!(
        text.contains(&format!(
            "\"secondary_mixes\": [\n      {first},\n      {second}\n    ],\n    \"display_outputs\": "
        )),
        "{text}"
    );

    for (from, to) in [
        ("\"number\": 2,", "\"number\": 1,"),
        ("\"number\": 2,", "\"number\": 17,"),
        ("\"number\": 2,", "\"number\": 3,"),
        ("\"mode\": \"preview_program\"", "\"mode\": \"preview\""),
        ("\"input\": 10", "\"input\": 99"),
        (
            "\"preview\": {\"input\": 11}",
            "\"preview\": {\"input\": 10}",
        ),
        (
            "\"preview\": {\"input\": 11}",
            "\"preview\": {\"input\": 12}",
        ),
        ("\"mix\": 2", "\"mix\": 9"),
        ("\"mix\": 2", "\"mix\": 3"),
        ("\"number\": 2,", "\"number\": 2, \"extra\": 1,"),
        (
            "\"mode\": \"output_only\",",
            "\"mode\": \"output_only\", \"preview\": null,",
        ),
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
fn chained_secondary_mix_cycles_are_refused_on_load() {
    let directory = TestDirectory::new("cycles");
    let store = ProjectStore::new(directory.path("show.freemix")).unwrap();
    let expected = configured();
    store.save(&stored(expected)).unwrap();
    let text = fs::read_to_string(directory.path("show.freemix/project.json")).unwrap();
    let broken = text
        .replacen(
            "\"mode\": \"preview_program\", \"program\": {\"input\": 10}, \"preview\": {\"input\": 11}",
            "\"mode\": \"preview_program\", \"program\": {\"mix\": 3}, \"preview\": {\"input\": 11}",
            1,
        )
        .replacen(
            "\"mode\": \"output_only\", \"program\": {\"mix\": 2}",
            "\"mode\": \"output_only\", \"program\": {\"mix\": 2}",
            1,
        );
    fs::write(directory.path("show.freemix/project.json"), broken).unwrap();
    assert!(store.load().is_err());
}

#[test]
fn setting_and_removing_a_secondary_mix_is_atomic() {
    let mut project = configured();
    let before = project.clone();
    assert!(
        project
            .set_secondary_mix(SecondaryMixConfig::output_only(
                number(4),
                "Broken",
                SecondaryMixSource::Input(input(99)),
            ))
            .is_err()
    );
    assert_eq!(project, before);

    assert!(project.remove_secondary_mix(number(3)).is_ok());
    assert!(project.remove_secondary_mix(number(2)).is_ok());
    assert!(project.secondary_mix(number(2)).is_none());
    // Mix 3 reads mix 2, so removing 2 first is refused.
    let mut project = configured();
    assert!(project.remove_secondary_mix(number(2)).is_err());
    assert!(project.secondary_mix(number(2)).is_some());
}

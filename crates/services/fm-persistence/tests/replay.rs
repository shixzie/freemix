//! OR-012: the replay recording settings (sources, retention, storage root)
//! and Replay inputs round-trip exactly and strictly in `project.json`;
//! recorded media and replay events are never part of the project.

use std::{
    fs,
    num::NonZeroU128,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use fm_model::{
    Input, InputKind, InputReference, Project, ProjectSettings, RemoveInputError, SimulatedAudio,
    SimulatedInput, SimulatedVideo,
};
use fm_persistence::{ProjectPosition, ProjectStore, RuntimeRouting, StoredProject};
use fm_types::replay::{ReplaySettings, ReplaySettingsError};
use fm_types::{
    AudioFormat, ChannelLayout, ColorMetadata, FrameRate, InputId, PixelFormat, ProjectId,
    SampleFormat, SampleRate, ScanMode, VideoDimensions, VideoFormat,
};

static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(name: &str) -> Self {
        let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "fm-persistence-replay-{}-{sequence}-{name}",
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

fn input_id(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn project() -> Project {
    let frame_rate = FrameRate::new(30, 1).unwrap();
    let mut project = Project::new(
        ProjectId::new(NonZeroU128::new(1).unwrap()),
        "Replay",
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
    for id in 1..=3 {
        project.add_input(Input {
            id: input_id(id),
            name: format!("Camera {id}"),
            kind: InputKind::Simulated(SimulatedInput::new(
                SimulatedVideo::Bars,
                SimulatedAudio::Silence,
            )),
            required_capabilities: Vec::new(),
        });
    }
    project.add_input(Input {
        id: input_id(9),
        name: "Replay A".to_owned(),
        kind: InputKind::Replay,
        required_capabilities: Vec::new(),
    });
    project
        .set_replay(ReplaySettings {
            sources: vec![input_id(3), input_id(1)],
            retention_seconds: 120,
            storage_root: Some("/srv/replay \"angles\"".to_owned()),
            auto_return: Some(12),
            transition: fm_types::replay::ReplayTransition::new(
                fm_types::replay::ReplayTransitionKind::Fade,
                30,
            )
            .unwrap(),
        })
        .unwrap();
    project
}

fn save(store: &ProjectStore, project: Project) {
    store
        .save(
            &StoredProject::from_project(
                project,
                RuntimeRouting::default(),
                ProjectPosition::default(),
                Vec::new(),
            )
            .unwrap(),
        )
        .unwrap();
}

#[test]
fn replay_settings_and_inputs_round_trip_exactly_and_are_strict() {
    let directory = TestDirectory::new("round-trip");
    let store = ProjectStore::new(directory.path("show.freemix")).unwrap();
    let expected = project();
    save(&store, expected.clone());
    let loaded = store.load().unwrap();
    assert_eq!(loaded.project(), &expected);
    assert_eq!(
        loaded.project().replay().sources,
        [input_id(3), input_id(1)]
    );
    let manifest = directory.path("show.freemix/project.json");
    let text = fs::read_to_string(&manifest).unwrap();
    for fragment in [
        "\"replay\": {\"sources\": [3, 1], \"retention_seconds\": 120, \"storage_root\": \
         \"/srv/replay \\\"angles\\\"\", \"auto_return\": 12, \"transition\": \"fade:30\"}",
        "\"kind\": {\"type\": \"replay\"}",
    ] {
        assert!(text.contains(fragment), "{fragment} in {text}");
    }

    for (from, to) in [
        // Validation: an unknown, repeated, or Replay source, a retention
        // out of range, and a relative root.
        ("\"sources\": [3, 1]", "\"sources\": [3, 7]"),
        ("\"sources\": [3, 1]", "\"sources\": [3, 3]"),
        ("\"sources\": [3, 1]", "\"sources\": [9]"),
        ("\"sources\": [3, 1]", "\"sources\": [1, 2, 3, 1, 2]"),
        ("\"retention_seconds\": 120", "\"retention_seconds\": 9"),
        ("\"retention_seconds\": 120", "\"retention_seconds\": 3601"),
        (
            "\"storage_root\": \"/srv/replay",
            "\"storage_root\": \"srv/replay",
        ),
        // Auto return (schema 88): at most 300 frames, a number or null,
        // never absent.
        ("\"auto_return\": 12", "\"auto_return\": 301"),
        ("\"auto_return\": 12", "\"auto_return\": \"12\""),
        (", \"auto_return\": 12", ""),
        // Syntax: every field is required, ids are nonzero numbers, and
        // nothing else is accepted.
        ("\"sources\": [3, 1]", "\"sources\": [0]"),
        ("\"sources\": [3, 1]", "\"sources\": [\"3\"]"),
        (", \"retention_seconds\": 120", ""),
        (
            "\"retention_seconds\": 120",
            "\"retention_seconds\": 120, \"events\": []",
        ),
        (
            "\"kind\": {\"type\": \"replay\"}",
            "\"kind\": {\"type\": \"replay\", \"event\": 1}",
        ),
    ] {
        assert!(text.contains(from), "{from}");
        fs::write(&manifest, text.replacen(from, to, 1)).unwrap();
        assert!(store.load().is_err(), "{from} -> {to} loaded");
    }
    // A null root lets the server choose.
    fs::write(
        &manifest,
        text.replacen(
            "\"storage_root\": \"/srv/replay \\\"angles\\\"\"",
            "\"storage_root\": null",
            1,
        ),
    )
    .unwrap();
    assert_eq!(store.load().unwrap().project().replay().storage_root, None);
}

#[test]
fn a_replay_source_cannot_be_removed_and_settings_are_checked() {
    let mut project = project();
    assert_eq!(
        project.input_reference(input_id(3)),
        Some(InputReference::ReplaySource)
    );
    assert_eq!(
        project.remove_input(input_id(3)),
        Err(RemoveInputError::DomainReference(input_id(3)))
    );
    assert_eq!(project.input_reference(input_id(2)), None);
    project.remove_input(input_id(2)).unwrap();
    let before = project.replay().clone();
    assert_eq!(
        project.set_replay(ReplaySettings {
            sources: vec![input_id(9)],
            ..ReplaySettings::default()
        }),
        Err(ReplaySettingsError::UnrecordableSource(input_id(9)))
    );
    assert_eq!(project.replay(), &before);
    // Turning replay off frees the sources.
    project.set_replay(ReplaySettings::default()).unwrap();
    project.remove_input(input_id(3)).unwrap();
    assert!(project.validate().is_ok());
}

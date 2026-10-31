//! OR-013: highlight reels round-trip exactly and strictly in `project.json`;
//! an empty reel list is omitted, and the schema gate is 83.

use std::{
    fs,
    num::NonZeroU128,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use fm_model::{
    Input, InputKind, Project, ProjectSettings, SimulatedAudio, SimulatedInput, SimulatedVideo,
};
use fm_persistence::{
    ProjectPosition, ProjectStore, ProjectValidationError, RuntimeRouting, StoreError,
    StoredProject,
};
use fm_types::reel::{MAX_REEL_TRIM_FRAMES, ReelClip, ReelMusic, ReelSpec};
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
            "fm-persistence-reel-{}-{sequence}-{name}",
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
        "Reels",
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
    project
}

fn clip(event: u32, angle: u128, trim_in: u32, trim_out: u32, speed: u8) -> ReelClip {
    ReelClip {
        event,
        angle: input_id(angle),
        trim_in_frames: trim_in,
        trim_out_frames: trim_out,
        speed_percent: speed,
        reverse: false,
        dissolve_frames: 0,
    }
}

fn reel(id: u64, name: &str, clips: Vec<ReelClip>) -> ReelSpec {
    ReelSpec {
        id,
        name: name.to_owned(),
        clips,
        music: None,
    }
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
fn reels_round_trip_exactly_and_strictly_in_project_json() {
    let directory = TestDirectory::new("round-trip");
    let store = ProjectStore::new(directory.path("show.freemix")).unwrap();
    let manifest = directory.path("show.freemix/project.json");

    // An empty reel list is omitted.
    let empty = project();
    save(&store, empty.clone());
    let text = fs::read_to_string(&manifest).unwrap();
    assert!(!text.contains("\"reels\""), "{text}");
    assert_eq!(store.load().unwrap().project(), &empty);

    let mut expected = project();
    expected
        .set_reel(reel(
            2,
            "Best \"goals\"",
            vec![
                clip(7, 3, 15, 30, 50),
                ReelClip {
                    dissolve_frames: 6,
                    ..clip(2, 1, 0, 12, 100)
                },
            ],
        ))
        .unwrap();
    expected
        .set_reel(ReelSpec {
            music: Some(ReelMusic {
                path: "/srv/music/warm \"up\".wav".to_owned(),
                level_percent: 80,
                duck_percent: 25,
            }),
            ..reel(1, "Warm-up", vec![clip(5, 2, 4, 0, 25)])
        })
        .unwrap();
    save(&store, expected.clone());
    let loaded = store.load().unwrap();
    assert_eq!(loaded.project(), &expected);
    assert_eq!(loaded.project().reels()[0].clips[0], clip(7, 3, 15, 30, 50));
    assert_eq!(loaded.project().reels()[1].clips[0], clip(5, 2, 4, 0, 25));

    let text = fs::read_to_string(&manifest).unwrap();
    let reels = "\"reels\": [{\"id\": 2, \"name\": \"Best \\\"goals\\\"\", \"clips\": \
                 [{\"event\": 7, \"angle\": 3, \"trim_in\": 15, \"trim_out\": 30, \"speed\": \
                 50, \"reverse\": false, \"dissolve\": 0}, {\"event\": 2, \"angle\": 1, \
                 \"trim_in\": 0, \"trim_out\": 12, \"speed\": 100, \"reverse\": false, \
                 \"dissolve\": 6}]}, {\"id\": 1, \"name\": \"Warm-up\", \
                 \"clips\": [{\"event\": 5, \"angle\": 2, \"trim_in\": 4, \"trim_out\": 0, \
                 \"speed\": 25, \"reverse\": false, \"dissolve\": 0}], \"music\": {\"path\": \
                 \"/srv/music/warm \\\"up\\\".wav\", \"level\": 80, \"duck\": 25}}]";
    assert!(text.contains(reels), "{text}");

    for (from, to) in [
        // Syntax: every field is required and nothing else is accepted.
        ("\"speed\": 50", "\"speed\": 50, \"fade\": 1"),
        (", \"trim_out\": 30", ""),
        (", \"reverse\": false", ""),
        (", \"dissolve\": 6", ""),
        (", \"duck\": 25", ""),
        ("\"duck\": 25", "\"duck\": 25, \"fade\": 1"),
        // A music bed with a relative path or a level over 100 (schema 86).
        ("\"path\": \"/srv", "\"path\": \"srv"),
        ("\"level\": 80", "\"level\": 101"),
        ("\"duck\": 25", "\"duck\": 101"),
        (
            "{\"id\": 1, \"name\": \"Warm-up\"",
            "{\"id\": 2, \"name\": \"Warm-up\"",
        ),
        // Validation: unknown angle, unsupported speed, blank name, and an
        // empty clip list.
        ("\"angle\": 3", "\"angle\": 9"),
        ("\"speed\": 50", "\"speed\": 60"),
        ("\"name\": \"Warm-up\"", "\"name\": \"  \""),
        // A first clip that dissolves, and a dissolve over ten seconds at
        // 60 frames per second (schema 84).
        (
            "\"speed\": 50, \"reverse\": false, \"dissolve\": 0",
            "\"speed\": 50, \"reverse\": false, \"dissolve\": 1",
        ),
        ("\"dissolve\": 6", "\"dissolve\": 601"),
        (
            "\"clips\": [{\"event\": 5, \"angle\": 2, \"trim_in\": 4, \"trim_out\": 0, \"speed\": \
             25, \"reverse\": false, \"dissolve\": 0}]",
            "\"clips\": []",
        ),
    ] {
        assert!(text.contains(from), "{from}");
        fs::write(&manifest, text.replacen(from, to, 1)).unwrap();
        assert!(store.load().is_err(), "{from} -> {to} loaded");
    }

    // Validation: a trim past the one-hour bound.
    let from = "\"trim_in\": 15";
    let to = format!("\"trim_in\": {}", MAX_REEL_TRIM_FRAMES + 1);
    assert!(text.contains(from), "{from}");
    fs::write(&manifest, text.replacen(from, &to, 1)).unwrap();
    assert!(store.load().is_err(), "{from} -> {to} loaded");

    // The previous schema is refused outright; no migration path exists.
    fs::write(
        &manifest,
        text.replacen("\"schema_version\": 102", "\"schema_version\": 89", 1),
    )
    .unwrap();
    assert!(matches!(
        store.load(),
        Err(StoreError::Validation(
            ProjectValidationError::UnsupportedSchema {
                found: 89,
                supported: 101
            }
        ))
    ));
    // The current schema loads.
    fs::write(&manifest, &text).unwrap();
    assert_eq!(store.load().unwrap().project(), &expected);
}

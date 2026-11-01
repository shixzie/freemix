//! SW-013: project-level production clock definitions round-trip exactly
//! and strictly in `project.json` (schema 106).

use std::{
    fs,
    num::NonZeroU128,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use fm_model::{Input, InputKind, Project, ProjectSettings};
use fm_persistence::{ProjectPosition, ProjectStore, RuntimeRouting, StoreError, StoredProject};
use fm_types::{
    AudioFormat, ChannelLayout, ColorMetadata, FrameRate, PixelFormat, ProjectClock,
    ProjectClockDirection, ProjectClockKind, ProjectClockOverrun, ProjectDurationFormat, ProjectId,
    ProjectSessionKind, ProjectTimeFormat, SampleFormat, SampleRate, ScanMode, VideoDimensions,
    VideoFormat,
};

static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(name: &str) -> Self {
        let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "fm-persistence-production-clocks-{}-{sequence}-{name}",
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

fn project() -> Project {
    let frame_rate = FrameRate::new(30, 1).unwrap();
    let mut project = Project::new(
        ProjectId::new(NonZeroU128::new(1).unwrap()),
        "Clocks",
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
    project.add_input(Input {
        id: fm_types::InputId::new(NonZeroU128::new(1).unwrap()),
        name: "Clip".into(),
        kind: InputKind::Media {
            asset_uri: "asset://clip.mp4".into(),
            playback: fm_model::MediaPlaybackSettings::default(),
        },
        required_capabilities: Vec::new(),
    });
    project
        .set_production_clocks(vec![
            ProjectClock {
                name: "Countdown".into(),
                kind: ProjectClockKind::Timer {
                    direction: ProjectClockDirection::CountDown,
                    preset_nanos: 300_000_000_000,
                    limit_nanos: None,
                    overrun: ProjectClockOverrun::StopAtTarget,
                    format: ProjectDurationFormat::MinutesSeconds,
                },
            },
            ProjectClock {
                name: "Local".into(),
                kind: ProjectClockKind::TimeOfDay {
                    zone: "EST5EDT,M3.2.0,M11.1.0".into(),
                    format: ProjectTimeFormat::Hours24,
                },
            },
            ProjectClock {
                name: "Media".into(),
                kind: ProjectClockKind::MediaRemaining {
                    input: Some(fm_types::InputId::new(NonZeroU128::new(1).unwrap())),
                    format: ProjectDurationFormat::HoursMinutesSecondsFrames {
                        numerator: 30,
                        denominator: 1,
                    },
                },
            },
            ProjectClock {
                name: "Live".into(),
                kind: ProjectClockKind::Session {
                    kind: ProjectSessionKind::Stream,
                    format: ProjectDurationFormat::Compact,
                },
            },
        ])
        .unwrap();
    project
}

fn stored(project: Project) -> Result<StoredProject, fm_persistence::ProjectValidationError> {
    StoredProject::from_project(
        project,
        RuntimeRouting::default(),
        ProjectPosition::default(),
        Vec::new(),
    )
}

#[test]
fn production_clocks_round_trip_exactly_and_strictly() {
    let directory = TestDirectory::new("round-trip");
    let store = ProjectStore::new(directory.path("show.freemix")).unwrap();
    let expected = project();
    store.save(&stored(expected.clone()).unwrap()).unwrap();
    assert_eq!(store.load().unwrap().project(), &expected);

    let text = fs::read_to_string(directory.path("show.freemix/project.json")).unwrap();
    assert!(
        text.contains("\"production_clocks\": [\n      {\"name\": \"Countdown\", \"kind\": \"timer\", \"direction\": \"count_down\", \"preset_ns\": 300000000000, \"limit_ns\": null, \"overrun\": \"stop_at_target\", \"format\": {\"type\": \"duration\", \"preset\": \"minutes_seconds\"}},"),
        "{text}"
    );
    assert!(
        text.contains("{\"name\": \"Local\", \"kind\": \"time_of_day\", \"zone\": \"EST5EDT,M3.2.0,M11.1.0\", \"format\": {\"type\": \"time_of_day\", \"preset\": \"hours_24\"}}"),
        "{text}"
    );

    for (from, to) in [
        ("\"production_clocks\": [", "\"production_clock\": ["),
        ("\"preset_ns\": 300000000000", "\"preset_ns\": -1"),
        ("\"limit_ns\": null", "\"limit_ns\": 1"),
        ("\"direction\": \"count_down\"", "\"direction\": \"down\""),
        (
            "\"zone\": \"EST5EDT,M3.2.0,M11.1.0\"",
            "\"zone\": \"EST5EDT, M3.2.0, M11.1.0\"",
        ),
        (
            "\"preset\": \"minutes_seconds\"",
            "\"preset\": \"fortnights\"",
        ),
        (
            "{\"name\": \"Media\", \"kind\": \"media_remaining\"",
            "{\"name\": \"Media\", \"kind\": \"media_remaining\", \"extra\": 1",
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
}

#[test]
fn duplicate_names_are_refused_before_storage() {
    let mut project = self::project();
    let mut clocks = project.production_clocks().to_vec();
    clocks[1].name = clocks[0].name.clone();
    assert!(project.set_production_clocks(clocks).is_err());
    assert_eq!(project.production_clocks().len(), 4);
}

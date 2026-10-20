//! GX-001: title timelines (the engine frame each shown title's tickers and
//! clocks count from) round-trip exactly and strictly in `project.json`
//! (schema 65); whether one names a title input is project validation.

use std::{
    collections::BTreeMap,
    fs,
    num::NonZeroU128,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use fm_model::{
    Input, InputKind, Project, ProjectSettings, TitleInput, TitleShownOn, TitleTimeline,
};
use fm_persistence::{ProjectPosition, ProjectStore, RuntimeRouting, StoreError, StoredProject};
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
            "fm-persistence-title-timelines-{}-{sequence}-{name}",
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

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn title(id: u128) -> Input {
    Input {
        id: input(id),
        name: format!("Title {id}"),
        kind: InputKind::Title(TitleInput {
            template_uri: "asset://titles/lower-third.fmtitle".into(),
            field_revision: 0,
            fields: Vec::new(),
        }),
        required_capabilities: Vec::new(),
    }
}

fn project() -> Project {
    let frame_rate = FrameRate::new(30, 1).unwrap();
    let mut project = Project::new(
        ProjectId::new(NonZeroU128::new(1).unwrap()),
        "Timelines",
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
    project.add_input(title(3));
    project.add_input(title(5));
    project.set_title_timelines(BTreeMap::from([
        (
            input(3),
            TitleTimeline {
                on: TitleShownOn::Air,
                since_frame: 18_446_744_073_709_551_615,
            },
        ),
        (
            input(5),
            TitleTimeline {
                on: TitleShownOn::Preview,
                since_frame: 0,
            },
        ),
    ]));
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
fn title_timelines_round_trip_exactly_and_strictly() {
    let directory = TestDirectory::new("round-trip");
    let store = ProjectStore::new(directory.path("show.freemix")).unwrap();
    let expected = project();
    store.save(&stored(expected.clone()).unwrap()).unwrap();
    assert_eq!(store.load().unwrap().project(), &expected);

    let text = fs::read_to_string(directory.path("show.freemix/project.json")).unwrap();
    assert!(
        text.contains(
            ",\n    \"title_timelines\": [\n      {\"input\": 3, \"on\": \"air\", \"since_frame\": \
             18446744073709551615},\n      {\"input\": 5, \"on\": \"preview\", \
             \"since_frame\": 0}\n    ]\n  },"
        ),
        "{text}"
    );

    for (from, to) in [
        ("\"title_timelines\": [", "\"title_timeline\": ["),
        ("\"on\": \"air\"", "\"on\": \"program\""),
        ("\"on\": \"air\", ", ""),
        ("\"since_frame\": 0}", "\"since_frame\": -1}"),
        (
            "\"since_frame\": 18446744073709551615",
            "\"since_frame\": 18446744073709551616",
        ),
        ("\"since_frame\": 0}", "\"since_frame\": 0, \"phase\": \"in\"}"),
        // Entries are strictly ascending by input.
        ("{\"input\": 5, ", "{\"input\": 3, "),
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
fn a_timeline_must_name_a_title_input() {
    let mut project = project();
    let mut timelines = project.title_timelines().clone();
    timelines.insert(
        input(9),
        TitleTimeline {
            on: TitleShownOn::Air,
            since_frame: 4,
        },
    );
    project.set_title_timelines(timelines);
    assert!(stored(project).is_err());

    // Removing a title input drops its timeline.
    let mut project = self::project();
    project.remove_input(input(5)).unwrap();
    assert_eq!(
        project.title_timelines().keys().copied().collect::<Vec<_>>(),
        vec![input(3)]
    );
    stored(project).unwrap();
}

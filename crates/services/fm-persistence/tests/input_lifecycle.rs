//! IN-026 at schema 37: the input category list and per-input lifecycle
//! (category, colour label, pause, close) round-trip exactly and strictly
//! in `project.json`.

use std::{
    fs,
    num::NonZeroU128,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use fm_model::{
    Input, InputCategory, InputCategoryId, InputColorLabel, InputKind, InputLifecycle, Project,
    ProjectSettings, SimulatedAudio, SimulatedInput, SimulatedVideo,
};
use fm_persistence::{ProjectPosition, ProjectStore, RuntimeRouting, StoredProject};
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
            "fm-persistence-lifecycle-{}-{sequence}-{name}",
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
    Project::new(
        ProjectId::new(NonZeroU128::new(1).unwrap()),
        "Lifecycle",
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
    )
}

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn category(value: u32) -> InputCategoryId {
    InputCategoryId::new(value).unwrap()
}

#[test]
fn input_lifecycle_round_trips_exactly_and_is_strict() {
    let directory = TestDirectory::new("lifecycle");
    let store = ProjectStore::new(directory.path("show.freemix")).unwrap();
    let mut expected = project();
    for (id, name) in [(1, "Camera"), (2, "Slides"), (3, "Guest")] {
        expected.add_input(Input {
            id: input(id),
            name: name.into(),
            kind: InputKind::Simulated(SimulatedInput::new(
                SimulatedVideo::Bars,
                SimulatedAudio::Silence,
            )),
            required_capabilities: Vec::new(),
        });
    }
    expected
        .set_input_categories(vec![
            InputCategory::new(category(4), "Cameras \"main\""),
            InputCategory::new(category(2), "Graphics"),
        ])
        .unwrap();
    expected
        .set_input_lifecycle(
            input(1),
            InputLifecycle {
                category: Some(category(4)),
                color: Some(InputColorLabel::Purple),
                paused: false,
                closed: false,
            },
        )
        .unwrap();
    expected
        .set_input_lifecycle(
            input(3),
            InputLifecycle {
                category: None,
                color: None,
                paused: true,
                closed: true,
            },
        )
        .unwrap();
    store
        .save(
            &StoredProject::from_project(
                expected.clone(),
                RuntimeRouting::default(),
                ProjectPosition::default(),
                Vec::new(),
            )
            .unwrap(),
        )
        .unwrap();
    let loaded = store.load().unwrap();
    assert_eq!(
        loaded.project().input_categories(),
        expected.input_categories()
    );
    assert_eq!(
        loaded.project().input_lifecycles(),
        expected.input_lifecycles()
    );
    let manifest = directory.path("show.freemix/project.json");
    let text = fs::read_to_string(&manifest).unwrap();
    assert!(text.contains("\"schema_version\": 63"));
    let fragment = "\"input_categories\": [{\"id\": 4, \"name\": \"Cameras \\\"main\\\"\"}, \
                    {\"id\": 2, \"name\": \"Graphics\"}],\n    \"input_lifecycle\": [\n      \
                    {\"input\": 1, \"category\": 4, \"color\": \"purple\", \"paused\": false, \
                    \"closed\": false},\n      \
                    {\"input\": 3, \"category\": null, \"color\": null, \"paused\": true, \
                    \"closed\": true}\n    ]";
    assert!(text.contains(fragment), "{text}");

    for (from, to) in [
        // Unknown colours and casing.
        ("\"purple\"", "\"magenta\""),
        ("\"purple\"", "\"Purple\""),
        // Category ids are nonzero and must exist.
        ("\"category\": 4", "\"category\": 0"),
        ("\"category\": 4", "\"category\": 9"),
        ("{\"id\": 2, ", "{\"id\": 0, "),
        // Category ids and names are unique.
        ("{\"id\": 2, ", "{\"id\": 4, "),
        ("\"Graphics\"", "\"cameras \\\"MAIN\\\"\""),
        ("\"Graphics\"", "\" \""),
        // Entries name existing inputs in ascending order.
        ("\"input\": 3,", "\"input\": 9,"),
        ("\"input\": 3,", "\"input\": 1,"),
        // An all-default entry is not written, so it is refused.
        (
            "\"category\": null, \"color\": null, \"paused\": true, \"closed\": true",
            "\"category\": null, \"color\": null, \"paused\": false, \"closed\": false",
        ),
        // Flags are booleans, and every field is required.
        ("\"paused\": true", "\"paused\": 1"),
        (", \"closed\": true}", "}"),
        ("\"input_lifecycle\"", "\"lifecycle\""),
        ("\"input_categories\"", "\"categories\""),
    ] {
        assert!(text.contains(from), "{from}");
        fs::write(&manifest, text.replacen(from, to, 1)).unwrap();
        assert!(store.load().is_err(), "{from} -> {to} loaded");
    }
}

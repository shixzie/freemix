//! RC-011 at schema 54: the project's scripts round-trip exactly and
//! strictly in `project.json`; bounds and duplicate ids are project
//! validation, and a script with a syntax error is stored as written.

use std::{
    fs,
    num::NonZeroU128,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use fm_model::{MAX_SCRIPT_SOURCE_BYTES, MAX_SCRIPTS, Project, ProjectSettings, Script};
use fm_persistence::{ProjectPosition, ProjectStore, RuntimeRouting, StoreError, StoredProject};
use fm_types::{
    AudioFormat, ChannelLayout, ColorMetadata, FrameRate, PixelFormat, ProjectId, SampleFormat,
    SampleRate, ScanMode, VideoDimensions, VideoFormat,
};

static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(name: &str) -> Self {
        let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "fm-persistence-scripts-{}-{sequence}-{name}",
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
        "Scripts",
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

fn stored(project: Project) -> Result<StoredProject, fm_persistence::ProjectValidationError> {
    StoredProject::from_project(
        project,
        RuntimeRouting::default(),
        ProjectPosition::default(),
        Vec::new(),
    )
}

fn scripts() -> Vec<Script> {
    vec![
        Script {
            id: 3,
            name: "Open \"show\"".into(),
            source:
                "# open\npreview 2\r\n\ttransition fade, 25\ntitle_text \"Lower\", 1, \"a\\\\b\""
                    .into(),
        },
        Script {
            id: 1,
            name: "Draft".into(),
            // A syntax error is stored as written.
            source: "if program ==\n".into(),
        },
    ]
}

#[test]
fn scripts_round_trip_exactly_and_strictly() {
    let directory = TestDirectory::new("round-trip");
    let store = ProjectStore::new(directory.path("show.freemix")).unwrap();
    let mut expected = project();
    expected.set_scripts(scripts());
    store.save(&stored(expected.clone()).unwrap()).unwrap();
    assert_eq!(store.load().unwrap().project(), &expected);

    let text = fs::read_to_string(directory.path("show.freemix/project.json")).unwrap();
    assert!(text.contains("\"schema_version\": 54"));
    assert!(
        text.contains(
            ",\n    \"scripts\": [\n      {\"id\": 3, \"name\": \"Open \\\"show\\\"\", \"source\": \
             \"# open\\npreview 2\\r\\n\\ttransition fade, 25\\ntitle_text \\\"Lower\\\", 1, \
             \\\"a\\\\\\\\b\\\"\"},\n      {\"id\": 1, \"name\": \"Draft\", \"source\": \"if \
             program ==\\n\"}\n    ]\n  },"
        ),
        "{text}"
    );

    // An empty script list is written as an empty array.
    store.save(&stored(project()).unwrap()).unwrap();
    let empty = fs::read_to_string(directory.path("show.freemix/project.json")).unwrap();
    assert!(empty.contains(",\n    \"scripts\": []\n  },"), "{empty}");

    store.save(&stored(expected).unwrap()).unwrap();
    for (from, to) in [
        ("\"scripts\": [", "\"script\": ["),
        ("{\"id\": 1, \"name\"", "{\"id\": \"1\", \"name\""),
        ("\"name\": \"Draft\"", "\"name\": 7"),
        ("\"source\": \"if", "\"code\": \"if"),
        ("\"name\": \"Draft\", ", ""),
        (
            "\"name\": \"Draft\",",
            "\"name\": \"Draft\", \"enabled\": true,",
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
    let without = text.replacen(",\n    \"scripts\": [", ",\n    \"ignored\": [", 1);
    fs::write(directory.path("show.freemix/project.json"), without).unwrap();
    assert!(matches!(
        store.load(),
        Err(StoreError::MalformedManifest { .. })
    ));

    // Bounds and duplicate ids are project validation.
    for (from, to) in [
        ("{\"id\": 1, \"name\"", "{\"id\": 3, \"name\""),
        ("{\"id\": 1, \"name\"", "{\"id\": 0, \"name\""),
        ("\"name\": \"Draft\"", "\"name\": \"  \""),
        ("\"name\": \"Draft\"", "\"name\": \"Dr\\u0007aft\""),
        ("\"source\": \"if", "\"source\": \"\\u0000if"),
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

#[test]
fn script_bounds_are_validated() {
    let mut project = project();
    project.set_scripts(
        (1..=u64::try_from(MAX_SCRIPTS).unwrap() + 1)
            .map(|id| Script {
                id,
                name: format!("S{id}"),
                source: String::new(),
            })
            .collect(),
    );
    assert!(stored(project.clone()).is_err());
    project.set_scripts(vec![Script {
        id: 1,
        name: "Long".into(),
        source: "#".repeat(MAX_SCRIPT_SOURCE_BYTES + 1),
    }]);
    assert!(stored(project.clone()).is_err());
    project.set_scripts(vec![Script {
        id: 1,
        name: "Long".into(),
        source: "#".repeat(MAX_SCRIPT_SOURCE_BYTES),
    }]);
    assert!(stored(project).is_ok());
}

//! IN-028: the PTZ preset virtual-input document round-trips exactly and
//! strictly in `project.json` (schema 106).

use std::{
    fs,
    num::NonZeroU128,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use fm_model::{Project, ProjectSettings};
use fm_persistence::{ProjectPosition, ProjectStore, RuntimeRouting, StoreError, StoredProject};
use fm_ptz::{
    CameraId, PresetId, PresetVirtualInput, PresetVirtualInputId, PresetVirtualInputs,
};
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
            "fm-persistence-ptz-presets-{}-{sequence}-{name}",
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
        "PTZ presets",
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

fn document() -> String {
    let mut inputs = PresetVirtualInputs::new();
    inputs
        .insert(
            PresetVirtualInput::new(PresetVirtualInputId::new(
                CameraId::new("cam-a").unwrap(),
                PresetId::new(1),
            ))
            .with_name("Wide")
            .with_thumbnail("asset://thumbs/wide.png")
            .enabled(true),
        )
        .unwrap();
    inputs
        .insert(
            PresetVirtualInput::new(PresetVirtualInputId::new(
                CameraId::new("cam-b").unwrap(),
                PresetId::new(2),
            ))
            .with_name("Close")
            .enabled(true),
        )
        .unwrap();
    inputs.to_document()
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
fn ptz_presets_round_trip_exactly_and_strictly() {
    let directory = TestDirectory::new("round-trip");
    let store = ProjectStore::new(directory.path("show.freemix")).unwrap();
    let mut expected = project();
    let canonical = document();
    expected
        .set_ptz_presets_document(canonical.clone())
        .unwrap();
    store.save(&stored(expected.clone()).unwrap()).unwrap();
    assert_eq!(store.load().unwrap().project(), &expected);

    let text = fs::read_to_string(directory.path("show.freemix/project.json")).unwrap();
    assert!(
        text.contains("\"ptz_presets\": \"freemix-ptz-presets 1\\ncount 2\\npreset x63616d2d61 1 1 x57696465 x61737365743a2f2f7468756d62732f776964652e706e67\\npreset x63616d2d62 2 1 x436c6f7365 none\\n\""),
        "{text}"
    );

    let escaped = canonical.replace('\n', "\\n");
    for (from, to) in [
        ("\"ptz_presets\": \"freemix-ptz-presets 1", "\"ptz_presets\": \"freemix-ptz-presets 2"),
        ("\\ncount 2", "\\ncount 1"),
        ("\\ncount 2", "\\ncount 3"),
        ("\\npreset x63616d2d62", "\\npreset x63616d2d60"),
        ("\\npreset x63616d2d61 1 1", "\\npreset x63616d2d61 1 2"),
        ("\\n\\n", "\\n"),
    ] {
        let malformed = text.replacen(&escaped, &to.replace('"', "\\\""), 1);
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
fn the_model_refuses_empty_and_oversized_documents() {
    let mut project = project();
    assert!(project.set_ptz_presets_document(String::new()).is_err());
    assert!(project
        .set_ptz_presets_document("x".repeat(256 * 1024 + 1))
        .is_err());
    assert!(project.set_ptz_presets_document(document()).is_ok());
    assert!(project.remove_ptz_presets_document().is_some());
    assert!(project.ptz_presets_document().is_none());
}

//! IN-014: a document input (a PDF or presentation asset and the page
//! on screen) round-trips exactly and strictly in `project.json`.

use std::{
    fs,
    num::NonZeroU128,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use fm_model::{DocumentInput, Input, InputKind, Project, ProjectSettings};
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
            "fm-persistence-documents-{}-{sequence}-{name}",
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
        "Documents",
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
    for (id, asset_uri, page) in [
        (1, "asset://decks/review.pdf", 7),
        (2, "asset://talk.pptx", 0),
    ] {
        project.add_input(Input {
            id: input_id(id),
            name: format!("Deck {id}"),
            kind: InputKind::Document(DocumentInput::new(asset_uri.to_owned(), page)),
            required_capabilities: Vec::new(),
        });
    }
    project
}

#[test]
fn document_inputs_round_trip_exactly_and_are_strict() {
    let directory = TestDirectory::new("round-trip");
    let store = ProjectStore::new(directory.path("show.freemix")).unwrap();
    let expected = project();
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
    assert_eq!(loaded.project(), &expected);
    assert_eq!(
        loaded.project().document(input_id(1)),
        Some(&DocumentInput::new(
            "asset://decks/review.pdf".to_owned(),
            7
        ))
    );
    let manifest = directory.path("show.freemix/project.json");
    let text = fs::read_to_string(&manifest).unwrap();
    let fragment =
        "{\"type\": \"document\", \"asset_uri\": \"asset://decks/review.pdf\", \"page\": 7}";
    assert!(text.contains(fragment), "{text}");

    for (from, to) in [
        // The page is below the most pages a document shows.
        ("\"page\": 7}", "\"page\": 10000}"),
        ("\"page\": 7}", "\"page\": -1}"),
        // Only a bundle asset of a document type.
        ("asset://decks/review.pdf", "asset://decks/review.mov"),
        ("asset://decks/review.pdf", "/home/show/review.pdf"),
        ("asset://decks/review.pdf", "asset://../review.pdf"),
        // Both fields are required, and nothing else is accepted.
        (", \"page\": 7}", "}"),
        ("\"page\": 7}", "\"page\": 7, \"notes\": true}"),
    ] {
        assert!(text.contains(from), "{from}");
        fs::write(&manifest, text.replacen(from, to, 1)).unwrap();
        assert!(store.load().is_err(), "{from} -> {to} loaded");
    }
}

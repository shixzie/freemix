//! GX-005: title preview-state documents round-trip exactly and strictly in
//! `project.json` (schema 106); whether one names a title input is project
//! validation.

use std::{
    collections::BTreeMap,
    fs,
    num::NonZeroU128,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use fm_data::{BoundValue, PreviewSnapshot, StagePolicy};
use fm_model::{Input, InputKind, Project, ProjectSettings, TitleInput};
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
            "fm-persistence-previews-{}-{sequence}-{name}",
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

fn color_input(id: u128) -> Input {
    Input {
        id: input(id),
        name: format!("Color {id}"),
        kind: InputKind::Color,
        required_capabilities: Vec::new(),
    }
}

fn project() -> Project {
    let frame_rate = FrameRate::new(30, 1).unwrap();
    let mut project = Project::new(
        ProjectId::new(NonZeroU128::new(1).unwrap()),
        "Previews",
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
    project.add_input(color_input(2));
    project
}

fn document() -> String {
    PreviewSnapshot {
        policy: StagePolicy::KeepLiveOnError,
        revision: 7,
        live: BTreeMap::from([
            ("home".to_owned(), BoundValue::Text("Home".into())),
            ("score".to_owned(), BoundValue::Number(3)),
            ("visible".to_owned(), BoundValue::Visible(true)),
        ]),
    }
    .to_document()
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
fn preview_documents_round_trip_exactly_and_strictly() {
    let directory = TestDirectory::new("round-trip");
    let store = ProjectStore::new(directory.path("show.freemix")).unwrap();
    let mut expected = project();
    let canonical = document();
    expected
        .set_preview_document(input(3), canonical.clone())
        .unwrap();
    store.save(&stored(expected.clone()).unwrap()).unwrap();
    assert_eq!(store.load().unwrap().project(), &expected);

    let text = fs::read_to_string(directory.path("show.freemix/project.json")).unwrap();
    assert!(
        text.contains("\"previews\": [\n      {\"input\": 3, \"document\": \"freemix-data-preview 1\\npolicy keep_live_on_error\\nrevision 7\\n"),
        "{text}"
    );

    let escaped = canonical.replace('\n', "\\n");
    for (from, to) in [
        ("\"previews\": [", "\"preview\": ["),
        ("\\nrevision 7", "\\nrevision -1"),
        ("\\npolicy keep_live_on_error", "\\npolicy keep_live"),
        ("\\nlive home text ", "\\nlive  home text "),
        (
            "\\nlive home text ",
            "\\nlive home text x, \\nlive home text ",
        ),
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
fn a_preview_must_name_a_title_input() {
    let mut project = project();
    project.restore_previews(BTreeMap::from([(input(9), document())]));
    assert!(stored(project).is_err());

    let mut project = self::project();
    assert!(project.set_preview_document(input(2), document()).is_err());
    assert!(project.set_preview_document(input(3), document()).is_ok());
    assert_eq!(project.preview_documents().len(), 1);

    // Removing the title drops its preview.
    project.remove_input(input(3)).unwrap();
    assert!(project.preview_documents().is_empty());
    stored(project).unwrap();
}

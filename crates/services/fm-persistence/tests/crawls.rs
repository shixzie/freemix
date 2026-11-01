//! GX-003: title crawl documents round-trip exactly and strictly in
//! `project.json` (schema 106).

use std::{
    fs,
    num::NonZeroU128,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use fm_model::{Input, InputKind, Project, ProjectSettings, TitleInput};
use fm_persistence::{ProjectPosition, ProjectStore, RuntimeRouting, StoreError, StoredProject};
use fm_titles::{
    Alignment, Bounds, Color, CrawlConfig, CrawlItemSnapshot, CrawlLogoSnapshot, CrawlMode,
    CrawlSnapshot, CrawlSpeed, FontStyle,
};
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
            "fm-persistence-crawls-{}-{sequence}-{name}",
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
            template_uri: "asset://titles/ticker.fmtitle".into(),
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
        "Crawls",
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
    project
}

fn document() -> String {
    CrawlSnapshot {
        config: CrawlConfig {
            mode: CrawlMode::Crawl,
            bounds: Bounds {
                x: 0,
                y: 900,
                width: 1_920,
                height: 120,
            },
            speed: CrawlSpeed::new(120, 1).unwrap(),
            frame_rate: FrameRate::new(30, 1).unwrap(),
            start_frame: 0,
            separator: " • ".into(),
            gap_px: 16,
            font: FontStyle {
                family: "Inter".into(),
                size_px: 48,
            },
            fill: Color::new(255, 255, 255, 255),
            background: None,
            opacity: 255,
            alignment: Alignment::default(),
        },
        items: vec![
            CrawlItemSnapshot {
                text: "Hello".into(),
                fill: None,
                logo: Some(CrawlLogoSnapshot {
                    asset: "asset://logos/news.png".into(),
                    width_px: 64,
                }),
            },
            CrawlItemSnapshot {
                text: "World".into(),
                fill: Some(Color::new(255, 0, 0, 255)),
                logo: None,
            },
        ],
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
fn crawl_documents_round_trip_exactly_and_strictly() {
    let directory = TestDirectory::new("round-trip");
    let store = ProjectStore::new(directory.path("show.freemix")).unwrap();
    let mut expected = project();
    let canonical = document();
    expected
        .set_crawl_document(input(3), canonical.clone())
        .unwrap();
    store.save(&stored(expected.clone()).unwrap()).unwrap();
    assert_eq!(store.load().unwrap().project(), &expected);

    let text = fs::read_to_string(directory.path("show.freemix/project.json")).unwrap();
    assert!(
        text.contains("\"crawls\": [\n      {\"input\": 3, \"document\": \"freemix-crawl 1\\nmode crawl\\nbounds 0 900 1920 120\\n"),
        "{text}"
    );

    let escaped = canonical.replace('\n', "\\n");
    for (from, to) in [
        ("\"crawls\": [", "\"crawl\": ["),
        ("freemix-crawl 1", "freemix-crawl 2"),
        ("\\nmode crawl", "\\nmode sideways"),
        ("\\nbounds 0 900 1920 120", "\\nbounds 0 900 0 120"),
        ("\\nspeed 120 1", "\\nspeed 0 1"),
        ("\\nframe_rate 30 1", "\\nframe_rate 30 0"),
        ("\\nitems 2", "\\nitems 3"),
        ("\\nopacity 255", "\\nopacity 256"),
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
fn a_crawl_must_name_a_title_input() {
    let mut project = project();
    project.restore_crawls(std::collections::BTreeMap::from([(input(9), document())]));
    assert!(stored(project).is_err());

    let mut project = self::project();
    assert!(project.set_crawl_document(input(9), document()).is_err());
    project.set_crawl_document(input(3), document()).unwrap();
    assert_eq!(project.crawl_documents().len(), 1);
    assert!(project.remove_crawl_document(input(3)).is_some());

    // Removing the title drops its crawl.
    project.set_crawl_document(input(3), document()).unwrap();
    project.remove_input(input(3)).unwrap();
    assert!(project.crawl_documents().is_empty());
    stored(project).unwrap();
}

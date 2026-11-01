use std::{
    collections::BTreeMap,
    fs,
    num::NonZeroU128,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use fm_data::{BoundValue, PreviewSnapshot, StagePolicy};
use fm_titles::{
    Alignment, Bounds, Color, CrawlConfig, CrawlItemSnapshot, CrawlLogoSnapshot, CrawlMode,
    CrawlSnapshot, CrawlSpeed, FontStyle,
};

use fm_model::{Input, InputKind, Project, ProjectSettings};
use fm_persistence::{
    AssetAuditIssue, AssetAuditReason, AssetResolveError, ProjectPosition, ProjectStore,
    RuntimeRouting, StoredProject,
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
            "fm-persistence-assets-{}-{sequence}-{name}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn store(&self) -> ProjectStore {
        ProjectStore::new(self.0.join("show.freemix")).unwrap()
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn create_asset(store: &ProjectStore, key: &str) -> PathBuf {
    let path = store.assets_root().join(key);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, b"asset").unwrap();
    path
}

fn input_id(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn stored_project(
    media_inputs: impl IntoIterator<Item = (u128, &'static str)>,
    non_media_input: (u128, InputKind),
) -> StoredProject {
    let frame_rate = FrameRate::new(30, 1).unwrap();
    let mut project = Project::new(
        ProjectId::new(NonZeroU128::new(1).unwrap()),
        "Asset audit",
        ProjectSettings {
            frame_rate,
            video: VideoFormat {
                dimensions: VideoDimensions::new(1, 1).unwrap(),
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
    for (id, kind) in media_inputs
        .into_iter()
        .map(|(id, asset_uri)| {
            (
                id,
                InputKind::Media {
                    asset_uri: asset_uri.into(),
                    playback: fm_model::MediaPlaybackSettings::default(),
                },
            )
        })
        .chain([non_media_input])
    {
        project.add_input(Input {
            id: input_id(id),
            name: format!("Input {id}"),
            kind,
            required_capabilities: Vec::new(),
        });
    }
    StoredProject::from_project(
        project,
        RuntimeRouting::default(),
        ProjectPosition::default(),
        Vec::new(),
    )
    .unwrap()
}

#[test]
fn project_asset_audit_reports_all_failures_in_stable_order() {
    let temp = TestDirectory::new("audit");
    let store = temp.store();
    create_asset(&store, "valid.mov");
    fs::create_dir_all(store.assets_root().join("directory")).unwrap();
    let inputs = vec![
        (50, "asset://missing.mov"),
        (40, "asset://valid.mov"),
        (20, "asset://directory"),
        (10, "asset://../invalid.mov"),
    ];
    #[cfg(unix)]
    let inputs = {
        use std::os::unix::fs::symlink;

        let mut inputs = inputs;
        let outside = temp.0.join("outside.mov");
        fs::write(&outside, b"outside").unwrap();
        symlink(&outside, store.assets_root().join("escape.mov")).unwrap();
        inputs.push((30, "asset://escape.mov"));
        inputs
    };
    let project = stored_project(
        inputs,
        (
            60,
            InputKind::NetworkStream(fm_types::network_stream::NetworkStreamConfig::new(
                "http://missing-network.test/live.ts",
            )),
        ),
    );

    let expected = vec![
        (10, AssetAuditReason::InvalidUri),
        (20, AssetAuditReason::NotRegularFile),
        (50, AssetAuditReason::MissingAsset),
    ];
    #[cfg(unix)]
    let expected = {
        let mut expected = expected;
        expected.push((30, AssetAuditReason::OutsideAssetsRoot));
        expected
    };
    let mut expected = expected;
    expected.sort_unstable_by_key(|(id, _)| *id);

    assert_eq!(
        store.audit_assets(&project),
        expected
            .into_iter()
            .map(|(id, reason)| AssetAuditIssue {
                input_id: input_id(id),
                reason,
            })
            .collect::<Vec<_>>()
    );
}

#[test]
fn a_folder_slideshow_audits_its_source_folder() {
    let temp = TestDirectory::new("folder-audit");
    let store = temp.store();
    fs::create_dir_all(store.assets_root().join("slides")).unwrap();
    fs::write(store.assets_root().join("picture.png"), b"asset").unwrap();
    let slideshow = |uri: &str| {
        InputKind::Slideshow(fm_types::still::SlideshowConfig::new(
            fm_types::still::SlideshowSource::Folder {
                asset_uri: uri.to_owned(),
            },
        ))
    };
    // A folder resolves; a file passed as a folder and a missing folder do
    // not.
    let project = stored_project([], (20, slideshow("asset://slides")));
    assert_eq!(store.audit_assets(&project), Vec::new());
    let project = stored_project([], (20, slideshow("asset://picture.png")));
    assert_eq!(
        store.audit_assets(&project),
        vec![AssetAuditIssue {
            input_id: input_id(20),
            reason: AssetAuditReason::NotRegularFile,
        }]
    );
    let project = stored_project([], (20, slideshow("asset://missing")));
    assert_eq!(
        store.audit_assets(&project),
        vec![AssetAuditIssue {
            input_id: input_id(20),
            reason: AssetAuditReason::MissingAsset,
        }]
    );
}

#[test]
fn resolves_nested_regular_file_beneath_assets_root() {
    let temp = TestDirectory::new("nested");
    let store = temp.store();
    let asset = create_asset(&store, "video/opening.mov");

    assert_eq!(
        store
            .resolve_asset_uri("asset://video/opening.mov")
            .unwrap(),
        fs::canonicalize(asset).unwrap()
    );
    assert_eq!(store.assets_root(), store.root().join("assets"));
}

#[test]
fn resolves_portable_unicode_filename() {
    let temp = TestDirectory::new("unicode");
    let store = temp.store();
    let asset = create_asset(&store, "stills/café.png");

    assert_eq!(
        store.resolve_asset_uri("asset://stills/café.png").unwrap(),
        fs::canonicalize(asset).unwrap()
    );
}

#[test]
fn rejects_invalid_uri_and_portable_path_tricks() {
    let temp = TestDirectory::new("invalid-uri");
    let store = temp.store();
    create_asset(&store, "valid.mov");
    let too_long = format!("asset://{}", "a".repeat(1017));
    let invalid = [
        "",
        "valid.mov",
        "file://valid.mov",
        "ASSET://valid.mov",
        "asset:/valid.mov",
        "asset://",
        "asset:///etc/passwd",
        "asset:////server/share/file.mov",
        "asset://.",
        "asset://..",
        "asset://./valid.mov",
        "asset://nested/../valid.mov",
        "asset://nested/./valid.mov",
        "asset://nested//valid.mov",
        "asset://nested/",
        "asset://..\\valid.mov",
        "asset://nested\\valid.mov",
        "asset://nested\\..\\valid.mov",
        "asset://C:/Windows/system.ini",
        "asset://C:relative.mov",
        "asset://nested/name:stream",
        "asset://\\\\server\\share\\file.mov",
        "asset://valid.mov?download=1",
        "asset://valid.mov#frame",
        "asset://%2e%2e/valid.mov",
        "asset://nested%2fvalid.mov",
        "asset://bad\0name.mov",
        "asset://bad\nname.mov",
        "asset://bad\u{7f}name.mov",
    ];

    for uri in invalid.into_iter().chain([too_long.as_str()]) {
        assert!(
            matches!(
                store.resolve_asset_uri(uri),
                Err(AssetResolveError::InvalidUri)
            ),
            "accepted {uri:?}"
        );
    }
}

#[test]
fn reports_missing_assets_root_and_file_without_creating_paths() {
    let temp = TestDirectory::new("missing");
    let store = temp.store();

    assert!(matches!(
        store.resolve_asset_uri("asset://missing.mov"),
        Err(AssetResolveError::AssetsRootUnavailable(_))
    ));
    assert!(!store.assets_root().exists());

    fs::create_dir_all(store.assets_root()).unwrap();
    assert!(matches!(
        store.resolve_asset_uri("asset://missing.mov"),
        Err(AssetResolveError::AssetUnavailable(_))
    ));
}

#[test]
fn rejects_directory_target() {
    let temp = TestDirectory::new("directory");
    let store = temp.store();
    fs::create_dir_all(store.assets_root().join("clips")).unwrap();

    assert!(matches!(
        store.resolve_asset_uri("asset://clips"),
        Err(AssetResolveError::NotRegularFile)
    ));
}

#[cfg(unix)]
#[test]
fn rejects_symlink_escape_after_canonicalization() {
    use std::os::unix::fs::symlink;

    let temp = TestDirectory::new("symlink-escape");
    let store = temp.store();
    fs::create_dir_all(store.assets_root()).unwrap();
    let outside = store.root().join("assets-escape").join("outside.mov");
    fs::create_dir_all(outside.parent().unwrap()).unwrap();
    fs::write(&outside, b"outside").unwrap();
    symlink(&outside, store.assets_root().join("escape.mov")).unwrap();

    assert!(matches!(
        store.resolve_asset_uri("asset://escape.mov"),
        Err(AssetResolveError::OutsideAssetsRoot)
    ));
}

#[test]
fn display_messages_do_not_expose_uri_or_filesystem_paths() {
    let temp = TestDirectory::new("secret-path-marker");
    let store = temp.store();
    let invalid = store
        .resolve_asset_uri("asset://secret-uri-marker/../file.mov")
        .unwrap_err();
    let missing_root = store
        .resolve_asset_uri("asset://secret-uri-marker.mov")
        .unwrap_err();

    for error in [&invalid, &missing_root] {
        let message = error.to_string();
        assert!(!message.contains("secret-uri-marker"));
        assert!(!message.contains("secret-path-marker"));
        assert!(!message.contains(temp.0.to_string_lossy().as_ref()));
    }
}

fn title_input() -> InputKind {
    InputKind::Title(fm_model::TitleInput {
        template_uri: "asset://titles/ticker.fmtitle".into(),
        field_revision: 0,
        fields: Vec::new(),
    })
}

fn crawl_document(logo: Option<&str>) -> String {
    CrawlSnapshot {
        config: CrawlConfig {
            mode: CrawlMode::Crawl,
            bounds: Bounds {
                x: 0,
                y: 0,
                width: 100,
                height: 100,
            },
            speed: CrawlSpeed::new(1, 1).unwrap(),
            frame_rate: FrameRate::new(30, 1).unwrap(),
            start_frame: 0,
            separator: " ".into(),
            gap_px: 0,
            font: FontStyle {
                family: "Inter".into(),
                size_px: 12,
            },
            fill: Color::new(255, 255, 255, 255),
            background: None,
            opacity: 255,
            alignment: Alignment::default(),
        },
        items: vec![CrawlItemSnapshot {
            text: "Item".into(),
            fill: None,
            logo: logo.map(|asset| CrawlLogoSnapshot {
                asset: asset.into(),
                width_px: 8,
            }),
        }],
    }
    .to_document()
}

fn preview_document(image: &str) -> String {
    PreviewSnapshot {
        policy: StagePolicy::RequireComplete,
        revision: 0,
        live: BTreeMap::from([("image".to_owned(), BoundValue::Image(image.into()))]),
    }
    .to_document()
}

#[test]
fn a_title_documents_logo_and_image_assets_are_audited() {
    let temp = TestDirectory::new("document-assets");
    let store = temp.store();
    create_asset(&store, "logos/present.png");
    create_asset(&store, "previews/present.png");
    let base = stored_project([], (60, InputKind::Color));
    let mut project = base.project().clone();
    for (id, name) in [(70, "Crawl"), (71, "Preview")] {
        project.add_input(Input {
            id: input_id(id),
            name: name.into(),
            kind: title_input(),
            required_capabilities: Vec::new(),
        });
    }
    project
        .set_crawl_document(
            input_id(70),
            crawl_document(Some("asset://logos/present.png")),
        )
        .unwrap();
    project
        .set_preview_document(
            input_id(71),
            preview_document("asset://previews/present.png"),
        )
        .unwrap();
    let count = |issues: &[AssetAuditIssue], id: u128, reason: AssetAuditReason| {
        issues
            .iter()
            .filter(|issue| issue.input_id == input_id(id) && issue.reason == reason)
            .count()
    };
    let stored = base.with_project(project.clone()).unwrap();
    let issues = store.audit_assets(&stored);
    // The title template is missing, but the present logo and image resolve.
    assert_eq!(
        count(&issues, 70, AssetAuditReason::MissingAsset),
        1,
        "{issues:?}"
    );
    assert_eq!(
        count(&issues, 71, AssetAuditReason::MissingAsset),
        1,
        "{issues:?}"
    );

    project
        .set_crawl_document(
            input_id(70),
            crawl_document(Some("asset://logos/missing.png")),
        )
        .unwrap();
    project
        .set_preview_document(
            input_id(71),
            preview_document("asset://previews/missing.png"),
        )
        .unwrap();
    let stored = base.with_project(project.clone()).unwrap();
    let issues = store.audit_assets(&stored);
    assert_eq!(
        count(&issues, 70, AssetAuditReason::MissingAsset),
        2,
        "{issues:?}"
    );
    assert_eq!(
        count(&issues, 71, AssetAuditReason::MissingAsset),
        2,
        "{issues:?}"
    );

    project.restore_crawls(BTreeMap::from([(input_id(70), "bad".into())]));
    project.restore_previews(BTreeMap::from([(input_id(71), "bad".into())]));
    let stored = base.with_project(project).unwrap();
    let issues = store.audit_assets(&stored);
    assert_eq!(
        count(&issues, 70, AssetAuditReason::UnreadableCrawlDocument),
        1,
        "{issues:?}"
    );
    assert_eq!(
        count(&issues, 71, AssetAuditReason::UnreadablePreviewDocument),
        1,
        "{issues:?}"
    );
}

//! RC-012 acceptance: portable bundles, templates, append, media import, and
//! missing-media relink.

use std::{
    fs,
    num::NonZeroU128,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use fm_model::{
    Input, InputAudioStripState, InputGainMilliDb, InputKind, Layer, LayerGeometry, Project,
    ProjectSettings, Rgba8, Rotation, Scene, SourceRef,
};
use fm_persistence::{
    AppendedAssetDisposition, AssetAuditIssue, AssetAuditReason, AssetDisposition, BundleError,
    IdempotencyReceipt, MutationBatch, ProjectPosition, ProjectStore, RelinkResolution,
    RelinkSearch, RenamedEntity, RuntimeRouting, SkippedContent, StoredProject,
};
use fm_types::{
    AudioFormat, ChannelLayout, ColorMetadata, FrameRate, InputId, PixelFormat, ProjectId,
    SampleFormat, SampleRate, ScanMode, SceneId, VideoDimensions, VideoFormat,
};

static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(name: &str) -> Self {
        let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "fm-persistence-bundles-{}-{sequence}-{name}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }

    fn store(&self, name: &str) -> ProjectStore {
        ProjectStore::new(self.0.join(name)).unwrap()
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.0.join(relative)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn write(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

fn write_asset(store: &ProjectStore, key: &str, bytes: &[u8]) {
    write(&store.assets_root().join(key), bytes);
}

fn nonzero(value: u128) -> NonZeroU128 {
    NonZeroU128::new(value).unwrap()
}

fn input_id(value: u128) -> InputId {
    InputId::new(nonzero(value))
}

fn scene_id(value: u128) -> SceneId {
    SceneId::new(nonzero(value))
}

fn empty_project(id: u128, name: &str) -> Project {
    let frame_rate = FrameRate::new(30, 1).unwrap();
    Project::new(
        ProjectId::new(nonzero(id)),
        name,
        ProjectSettings {
            frame_rate,
            video: VideoFormat {
                dimensions: VideoDimensions::new(16, 9).unwrap(),
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

fn media(id: u128, name: &str, uri: &str) -> Input {
    Input {
        id: input_id(id),
        name: name.into(),
        kind: InputKind::Media {
            asset_uri: uri.into(),
            playback: fm_model::MediaPlaybackSettings::default(),
        },
        required_capabilities: Vec::new(),
    }
}

fn media_project(id: u128, inputs: &[(u128, &str)]) -> Project {
    let mut project = empty_project(id, "Show");
    for (input, uri) in inputs {
        project.add_input(media(*input, &format!("Input {input}"), uri));
    }
    project
}

fn stored(project: Project) -> StoredProject {
    StoredProject::from_project(
        project,
        RuntimeRouting::default(),
        ProjectPosition::default(),
        Vec::new(),
    )
    .unwrap()
}

fn layer(name: &str, source: SourceRef) -> Layer {
    Layer {
        id: next_layer_id(),
        name: name.into(),
        source,
        enabled: true,
        geometry: LayerGeometry::new(0, 0, 16, 9, Rotation::Deg0),
        crop: None,
        mask: None,
        key: None,
        effects: Vec::new(),
        opacity: 255,
        z_order: 0,
    }
}

fn uri_of(project: &StoredProject, input: u128) -> &str {
    match &project
        .project()
        .inputs()
        .iter()
        .find(|candidate| candidate.id == input_id(input))
        .unwrap()
        .kind
    {
        InputKind::Media { asset_uri, .. } => asset_uri,
        other => panic!("input {input} is not media: {other:?}"),
    }
}

/// Lists every path under `root`, relative and sorted, with `/` separators.
fn tree(root: &Path) -> Vec<String> {
    fn walk(root: &Path, directory: &Path, out: &mut Vec<String>) {
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            let relative = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if fs::symlink_metadata(&path).unwrap().is_dir() {
                out.push(format!("{relative}/"));
                walk(root, &path, out);
            } else {
                out.push(relative);
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

fn sibling_names(directory: &Path) -> Vec<String> {
    let mut names = fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    names.sort();
    names
}

// ---------------------------------------------------------------------------
// Portable bundles
// ---------------------------------------------------------------------------

#[test]
fn portable_bundle_copies_only_referenced_media_and_reopens_identically() {
    let temp = TestDirectory::new("portable");
    let source = temp.store("show.freemix");
    let project = stored(media_project(
        1,
        &[
            (1, "asset://clips/intro.mov"),
            (2, "asset://logo.png"),
            (3, "asset://logo.png"),
        ],
    ));
    source.save(&project).unwrap();
    write_asset(&source, "clips/intro.mov", b"intro");
    write_asset(&source, "logo.png", b"logo");
    write_asset(&source, "unused.wav", b"unused");
    source.write_autosave(&project, 2).unwrap();

    fs::create_dir(temp.path("export")).unwrap();
    let destination = ProjectStore::new(temp.path("export/show.freemix")).unwrap();
    let export = source.export_portable_bundle(&destination).unwrap();

    assert_eq!(export.project, project);
    assert_eq!(export.rewritten_inputs, Vec::<InputId>::new());
    let summary = export
        .assets
        .iter()
        .map(|asset| {
            (
                asset.source_uri.as_str(),
                asset.bundled_uri.as_str(),
                asset.bytes,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        summary,
        [
            ("asset://clips/intro.mov", "asset://clips/intro.mov", 5),
            ("asset://logo.png", "asset://logo.png", 4),
        ]
    );
    assert_eq!(
        tree(destination.root()),
        [
            "assets/",
            "assets/clips/",
            "assets/clips/intro.mov",
            "assets/logo.png",
            "project.json",
        ]
    );
    assert_eq!(destination.load().unwrap(), project);
    assert_eq!(destination.audit_assets(&project), Vec::new());
    assert_eq!(sibling_names(&temp.path("export")), ["show.freemix"]);
    // The source is untouched.
    assert_eq!(source.load().unwrap(), project);
    assert!(source.assets_root().join("unused.wav").exists());
}

/// A folder slideshow (IN-009) shows every picture file directly inside its
/// asset folder, so a self-contained copy carries those pictures under the
/// same folder; other files in the folder stay behind.
#[test]
fn portable_bundle_copies_a_folder_slideshows_pictures() {
    use fm_types::still::{SlideshowConfig, SlideshowSource};

    let temp = TestDirectory::new("portable-folder-slideshow");
    let source = temp.store("show.freemix");
    let mut project = empty_project(1, "Show");
    project.add_input(Input {
        id: input_id(1),
        name: "Slides".into(),
        kind: InputKind::Slideshow(SlideshowConfig::new(SlideshowSource::Folder {
            asset_uri: "asset://slides".into(),
        })),
        required_capabilities: Vec::new(),
    });
    let project = stored(project);
    source.save(&project).unwrap();
    write_asset(&source, "slides/1.png", b"one");
    write_asset(&source, "slides/2.jpg", b"two");
    write_asset(&source, "slides/notes.txt", b"notes");
    write_asset(&source, "slides/nested/3.png", b"nested");
    write_asset(&source, "unused.png", b"unused");

    fs::create_dir(temp.path("export")).unwrap();
    let destination = ProjectStore::new(temp.path("export/show.freemix")).unwrap();
    let export = source.export_portable_bundle(&destination).unwrap();

    assert_eq!(export.project, project);
    assert_eq!(
        tree(destination.root()),
        [
            "assets/",
            "assets/slides/",
            "assets/slides/1.png",
            "assets/slides/2.jpg",
            "project.json",
        ]
    );
    assert_eq!(
        fs::read(destination.assets_root().join("slides/2.jpg")).unwrap(),
        b"two"
    );
}

#[test]
fn portable_bundle_rewrites_keys_that_clash_or_are_illegal_elsewhere() {
    let temp = TestDirectory::new("portable-keys");
    let source = temp.store("show.freemix");
    let project = stored(media_project(
        1,
        &[
            (1, "asset://Logo.png"),
            (2, "asset://logo.png"),
            (3, "asset://aux.mov"),
            (4, "asset://dots./clip "),
            (5, "asset://Media/a.mov"),
            (6, "asset://media"),
        ],
    ));
    source.save(&project).unwrap();
    write_asset(&source, "Logo.png", b"upper");
    write_asset(&source, "logo.png", b"lower");
    write_asset(&source, "aux.mov", b"aux");
    write_asset(&source, "dots./clip ", b"dots");
    write_asset(&source, "Media/a.mov", b"a");
    write_asset(&source, "media", b"file");

    let destination = temp.store("portable.freemix");
    let export = source.export_portable_bundle(&destination).unwrap();

    let reopened = destination.load().unwrap();
    assert_eq!(reopened, export.project);
    assert_eq!(uri_of(&reopened, 1), "asset://Logo.png");
    assert_eq!(uri_of(&reopened, 2), "asset://logo-2.png");
    assert_eq!(uri_of(&reopened, 3), "asset://_aux.mov");
    assert_eq!(uri_of(&reopened, 4), "asset://dots_/clip_");
    assert_eq!(uri_of(&reopened, 5), "asset://Media/a.mov");
    // `media` as a file would clash with the `Media/` directory when folded.
    assert_eq!(uri_of(&reopened, 6), "asset://media-2");
    assert_eq!(
        export.rewritten_inputs,
        [input_id(2), input_id(3), input_id(4), input_id(6)]
    );
    for (input, bytes) in [
        (1, &b"upper"[..]),
        (2, b"lower"),
        (3, b"aux"),
        (4, b"dots"),
        (5, b"a"),
        (6, b"file"),
    ] {
        let path = destination
            .resolve_asset_uri(uri_of(&reopened, input))
            .unwrap();
        assert_eq!(fs::read(path).unwrap(), bytes, "input {input}");
    }
    // Every bundled key is distinct ignoring case.
    let mut folded = export
        .assets
        .iter()
        .map(|asset| asset.bundled_uri.to_lowercase())
        .collect::<Vec<_>>();
    folded.sort();
    folded.dedup();
    assert_eq!(folded.len(), export.assets.len());
}

#[test]
fn portable_bundle_never_overwrites_and_requires_resolvable_media() {
    let temp = TestDirectory::new("portable-refusals");
    let source = temp.store("show.freemix");
    source
        .save(&stored(media_project(1, &[(1, "asset://a.mov")])))
        .unwrap();
    write_asset(&source, "a.mov", b"a");

    // An existing destination, even an empty directory, is never replaced.
    let occupied = temp.store("occupied.freemix");
    fs::create_dir(occupied.root()).unwrap();
    assert!(matches!(
        source.export_portable_bundle(&occupied),
        Err(BundleError::DestinationExists(path)) if path == occupied.root()
    ));
    assert_eq!(tree(occupied.root()), Vec::<String>::new());

    // A destination inside the source bundle is refused.
    let nested = ProjectStore::new(source.root().join("assets/copy.freemix")).unwrap();
    assert!(matches!(
        source.export_portable_bundle(&nested),
        Err(BundleError::DestinationInsideSource(_))
    ));
    assert!(!nested.root().exists());

    // Missing media, `..` traversal, and an escaping symlink are all refused.
    let mut inputs = vec![
        (1, "asset://a.mov"),
        (2, "asset://missing.mov"),
        (3, "asset://../show.freemix/project.json"),
    ];
    #[cfg(unix)]
    {
        write(&temp.path("outside.mov"), b"outside");
        std::os::unix::fs::symlink(
            temp.path("outside.mov"),
            source.assets_root().join("esc.mov"),
        )
        .unwrap();
        inputs.push((4, "asset://esc.mov"));
    }
    source.save(&stored(media_project(1, &inputs))).unwrap();
    let destination = temp.store("out.freemix");
    let Err(BundleError::UnresolvedAssets(issues)) = source.export_portable_bundle(&destination)
    else {
        panic!("unresolved media must be refused");
    };
    let mut expected = vec![
        AssetAuditIssue {
            input_id: input_id(2),
            reason: AssetAuditReason::MissingAsset,
        },
        AssetAuditIssue {
            input_id: input_id(3),
            reason: AssetAuditReason::InvalidUri,
        },
    ];
    #[cfg(unix)]
    expected.push(AssetAuditIssue {
        input_id: input_id(4),
        reason: AssetAuditReason::OutsideAssetsRoot,
    });
    assert_eq!(issues, expected);
    assert!(!destination.root().exists());
    assert!(
        sibling_names(&temp.0)
            .iter()
            .all(|name| !name.contains("staging")),
        "no staging directory may survive a refusal"
    );
}

#[cfg(unix)]
#[test]
fn portable_bundle_materializes_in_bundle_symlinks_as_regular_files() {
    let temp = TestDirectory::new("portable-symlink");
    let source = temp.store("show.freemix");
    source
        .save(&stored(media_project(
            1,
            &[(1, "asset://real.mov"), (2, "asset://alias.mov")],
        )))
        .unwrap();
    write_asset(&source, "real.mov", b"real");
    std::os::unix::fs::symlink("real.mov", source.assets_root().join("alias.mov")).unwrap();

    let destination = temp.store("copy.freemix");
    source.export_portable_bundle(&destination).unwrap();
    let alias = destination.assets_root().join("alias.mov");
    assert!(fs::symlink_metadata(&alias).unwrap().file_type().is_file());
    assert_eq!(fs::read(alias).unwrap(), b"real");
}

#[test]
fn bundle_copies_refuse_a_source_with_unapplied_journal_batches() {
    let temp = TestDirectory::new("portable-journal");
    let source = temp.store("show.freemix");
    source.save(&stored(media_project(1, &[]))).unwrap();
    source
        .append_batch(&MutationBatch::new(1, 0, 1, vec![1, 2, 3]))
        .unwrap();

    let destination = temp.store("copy.freemix");
    assert!(matches!(
        source.export_portable_bundle(&destination),
        Err(BundleError::UnappliedJournal { batches: 1 })
    ));
    assert!(matches!(
        source.instantiate_template(&destination, ProjectId::new(nonzero(9)), "New"),
        Err(BundleError::UnappliedJournal { batches: 1 })
    ));
    let target = temp.store("target.freemix");
    let target_project = stored(media_project(2, &[]));
    assert!(matches!(
        target.append_project(&target_project, &source),
        Err(BundleError::UnappliedJournal { batches: 1 })
    ));
    assert!(!destination.root().exists());
}

// ---------------------------------------------------------------------------
// Templates
// ---------------------------------------------------------------------------

#[test]
fn template_instantiation_assigns_fresh_identity_and_drops_history() {
    let temp = TestDirectory::new("template");
    let template = temp.store("lower-thirds.freemix");
    let template_project = StoredProject::from_project(
        media_project(1, &[(1, "asset://bg.png")]),
        RuntimeRouting {
            desired_program_id: Some(input_id(1)),
            realized_program_id: Some(input_id(1)),
            ..RuntimeRouting::default()
        },
        ProjectPosition {
            revision: 5,
            state_epoch: 2,
            event_sequence: 9,
            frames_rendered: 300,
            runtime_generation: 3,
            clock_time_nanos: 10_000,
        },
        vec![IdempotencyReceipt::accepted("key", "command", 4, 120)],
    )
    .unwrap();
    template.save(&template_project).unwrap();
    write_asset(&template, "bg.png", b"background");

    let destination = temp.store("tonight.freemix");
    let fresh = ProjectId::new(nonzero(77));
    for (id, name, expected) in [
        (ProjectId::new(nonzero(1)), "Tonight", "identity"),
        (fresh, "   ", "name"),
    ] {
        let result = template.instantiate_template(&destination, id, name);
        match (expected, result) {
            ("identity", Err(BundleError::TemplateIdentityReused))
            | ("name", Err(BundleError::EmptyName)) => {}
            (_, other) => panic!("unexpected {other:?}"),
        }
        assert!(!destination.root().exists());
    }

    let created = template
        .instantiate_template(&destination, fresh, "Tonight")
        .unwrap();
    let reopened = destination.load().unwrap();
    assert_eq!(reopened, created.project);
    assert_eq!(reopened.project().id(), fresh);
    assert_eq!(reopened.show_name(), "Tonight");
    assert_eq!(reopened.position(), ProjectPosition::default());
    assert_eq!(
        reopened.idempotency_receipts(),
        &[] as &[IdempotencyReceipt]
    );
    assert_eq!(
        reopened.project().inputs(),
        template_project.project().inputs()
    );
    assert_eq!(
        reopened.runtime_routing(),
        template_project.runtime_routing()
    );
    assert_eq!(
        fs::read(destination.resolve_asset_uri("asset://bg.png").unwrap()).unwrap(),
        b"background"
    );
    // The template itself is unchanged and can be instantiated again.
    assert_eq!(template.load().unwrap(), template_project);
    assert!(matches!(
        template.instantiate_template(&destination, ProjectId::new(nonzero(78)), "Again"),
        Err(BundleError::DestinationExists(_))
    ));
}

// ---------------------------------------------------------------------------
// Append
// ---------------------------------------------------------------------------

#[test]
#[allow(clippy::too_many_lines)] // one end-to-end append scenario
fn append_remaps_ids_resolves_name_clashes_and_carries_media() {
    let temp = TestDirectory::new("append");
    let target_store = temp.store("target.freemix");
    let mut target = media_project(1, &[(1, "asset://logo.png")]);
    target.rename_input(input_id(1), "Cam".into()).unwrap();
    target
        .add_scene_input_checked(scene_id(1), "Main".into(), input_id(2), "Main".into())
        .unwrap();
    let target = stored(target);
    target_store.save(&target).unwrap();
    write_asset(&target_store, "logo.png", b"target logo");
    write_asset(&target_store, "same.png", b"shared");
    write_asset(&target_store, "gone.mov", b"unrelated target file");

    let source_store = temp.store("source.freemix");
    let mut source = empty_project(2, "Source");
    source.add_input(media(10, "Cam", "asset://logo.png"));
    source.add_input(media(11, "Clip", "asset://clips/clip.mov"));
    source.add_input(media(12, "Shared", "asset://same.png"));
    source.add_input(media(13, "Gone", "asset://gone.mov"));
    source.add_scene(Scene {
        id: scene_id(7),
        name: "Lower".into(),
        background: Rgba8::OPAQUE_BLACK,
        layers: vec![layer("clip", SourceRef::Input(input_id(11)))],
        animation: fm_model::SceneAnimation::default(),
    });
    source.add_scene(Scene {
        id: scene_id(5),
        name: "MAIN".into(),
        background: Rgba8::OPAQUE_BLACK,
        layers: vec![
            layer("cam", SourceRef::Input(input_id(10))),
            layer("lower", SourceRef::Scene(scene_id(7))),
        ],
        animation: fm_model::SceneAnimation::default(),
    });
    source.add_input(Input {
        id: input_id(14),
        name: "Main".into(),
        kind: InputKind::Scene {
            scene_id: scene_id(5),
            audio_source: Some(input_id(11)),
        },
        required_capabilities: Vec::new(),
    });
    let strip = InputAudioStripState {
        gain: InputGainMilliDb::new(-6_000).unwrap(),
        muted: true,
        ..InputAudioStripState::default()
    };
    assert!(source.set_input_audio_strip(input_id(11), strip));
    source_store.save(&stored(source)).unwrap();
    write_asset(&source_store, "logo.png", b"source logo");
    write_asset(&source_store, "clips/clip.mov", b"clip");
    write_asset(&source_store, "same.png", b"shared");

    let (appended, report) = target_store
        .append_project(&target_store.load().unwrap(), &source_store)
        .unwrap();

    assert_eq!(
        report.inputs,
        [
            (input_id(10), input_id(3)),
            (input_id(11), input_id(4)),
            (input_id(12), input_id(5)),
            (input_id(13), input_id(6)),
            (input_id(14), input_id(7)),
        ]
    );
    assert_eq!(
        report.scenes,
        [(scene_id(7), scene_id(2)), (scene_id(5), scene_id(3))]
    );
    assert_eq!(
        report.renamed_inputs,
        [
            RenamedEntity {
                source_id: input_id(10),
                from: "Cam".into(),
                to: "Cam (2)".into(),
            },
            RenamedEntity {
                source_id: input_id(14),
                from: "Main".into(),
                to: "Main (2)".into(),
            },
        ]
    );
    assert_eq!(
        report.renamed_scenes,
        [RenamedEntity {
            source_id: scene_id(5),
            from: "MAIN".into(),
            to: "MAIN (2)".into(),
        }]
    );
    let assets = report
        .assets
        .iter()
        .map(|asset| {
            (
                asset.source_uri.as_str(),
                asset.target_uri.as_str(),
                asset.disposition,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        assets,
        [
            (
                "asset://clips/clip.mov",
                "asset://clips/clip.mov",
                AppendedAssetDisposition::Placed(AssetDisposition::Copied { bytes: 4 }),
            ),
            (
                "asset://gone.mov",
                "asset://gone-2.mov",
                AppendedAssetDisposition::MissingInSource,
            ),
            (
                "asset://logo.png",
                "asset://logo-2.png",
                AppendedAssetDisposition::Placed(AssetDisposition::Copied { bytes: 11 }),
            ),
            (
                "asset://same.png",
                "asset://same.png",
                AppendedAssetDisposition::Placed(AssetDisposition::Reused),
            ),
        ]
    );
    assert_eq!(report.skipped, SkippedContent::default());

    // The merged project validates, saves, and keeps the remapped graph.
    target_store.save(&appended).unwrap();
    let reopened = target_store.load().unwrap();
    assert_eq!(reopened, appended);
    let project = reopened.project();
    assert_eq!(project.inputs().len(), 7);
    assert_eq!(project.inputs()[0..2], target.project().inputs()[..]);
    assert_eq!(uri_of(&reopened, 3), "asset://logo-2.png");
    assert_eq!(uri_of(&reopened, 6), "asset://gone-2.mov");
    assert_eq!(project.input_audio_strip(input_id(4)), Some(strip));
    assert_eq!(
        project.inputs()[6].kind,
        InputKind::Scene {
            scene_id: scene_id(3),
            audio_source: Some(input_id(4)),
        }
    );
    let main_two = project
        .scenes()
        .iter()
        .find(|scene| scene.id == scene_id(3))
        .unwrap();
    assert_eq!(
        main_two
            .layers
            .iter()
            .map(|layer| layer.source)
            .collect::<Vec<_>>(),
        [SourceRef::Input(input_id(3)), SourceRef::Scene(scene_id(2))]
    );
    assert_eq!(
        fs::read(target_store.resolve_asset_uri("asset://logo.png").unwrap()).unwrap(),
        b"target logo"
    );
    assert_eq!(
        fs::read(
            target_store
                .resolve_asset_uri("asset://logo-2.png")
                .unwrap()
        )
        .unwrap(),
        b"source logo"
    );
    // The missing source asset never binds to the unrelated target file.
    assert_eq!(
        target_store.audit_assets(&reopened),
        [AssetAuditIssue {
            input_id: input_id(6),
            reason: AssetAuditReason::MissingAsset,
        }]
    );
    assert_eq!(reopened.position(), target.position());
}

/// An appended folder slideshow (IN-009) shows its own pictures: the
/// folder is copied under a fresh folder key, never bound to the target's
/// unrelated folder of the same name.
#[test]
fn append_copies_a_folder_slideshows_pictures_into_a_fresh_folder() {
    use fm_types::still::{SlideshowConfig, SlideshowSource};

    let slideshow = |id: u128, folder: &str| Input {
        id: input_id(id),
        name: format!("Slides {id}"),
        kind: InputKind::Slideshow(SlideshowConfig::new(SlideshowSource::Folder {
            asset_uri: folder.into(),
        })),
        required_capabilities: Vec::new(),
    };
    let folder_of = |project: &StoredProject, input: u128| match &project
        .project()
        .inputs()
        .iter()
        .find(|candidate| candidate.id == input_id(input))
        .unwrap()
        .kind
    {
        InputKind::Slideshow(config) => match &config.source {
            SlideshowSource::Folder { asset_uri } => asset_uri.clone(),
            SlideshowSource::List(_) => panic!("not a folder"),
        },
        other => panic!("not a slideshow: {other:?}"),
    };

    let temp = TestDirectory::new("append-folder-slideshow");
    let target_store = temp.store("target.freemix");
    let mut target = empty_project(1, "Target");
    target.add_input(slideshow(1, "asset://slides"));
    target_store.save(&stored(target)).unwrap();
    write_asset(&target_store, "slides/target.png", b"target picture");

    let source_store = temp.store("source.freemix");
    let mut source = empty_project(2, "Source");
    source.add_input(slideshow(10, "asset://slides"));
    source_store.save(&stored(source)).unwrap();
    write_asset(&source_store, "slides/a.png", b"source a");
    write_asset(&source_store, "slides/b.jpg", b"source b");
    write_asset(&source_store, "slides/readme.txt", b"not a picture");

    let (appended, _report) = target_store
        .append_project(&target_store.load().unwrap(), &source_store)
        .unwrap();
    // The target's own slideshow is untouched.
    assert_eq!(folder_of(&appended, 1), "asset://slides");
    let folder = folder_of(&appended, 2);
    assert_ne!(folder, "asset://slides");
    let key = folder.strip_prefix("asset://").unwrap();
    let mut names = fs::read_dir(target_store.assets_root().join(key))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect::<Vec<_>>();
    names.sort();
    assert_eq!(names, ["a.png", "b.jpg"]);
    assert_eq!(
        fs::read(target_store.assets_root().join(key).join("b.jpg")).unwrap(),
        b"source b"
    );
}

#[test]
fn append_never_binds_a_missing_source_asset_to_another_appended_asset() {
    let temp = TestDirectory::new("append-claimed-keys");
    let target_store = temp.store("target.freemix");
    let target = stored(media_project(1, &[(1, "asset://clip")]));
    target_store.save(&target).unwrap();
    write_asset(&target_store, "clip", b"target clip");

    // `clip` sorts before `clip-2`, so the missing `clip` picks its key
    // before the present source asset `clip-2` is placed. Neither may end up
    // sharing a key with another appended asset.
    let source_store = temp.store("source.freemix");
    let mut source = empty_project(2, "Source");
    source.add_input(media(10, "Missing", "asset://clip"));
    source.add_input(media(11, "Present", "asset://clip-2"));
    source.add_input(media(12, "Missing too", "asset://clip-3"));
    source_store.save(&stored(source)).unwrap();
    write_asset(&source_store, "clip-2", b"present source clip");

    let (appended, report) = target_store
        .append_project(&target_store.load().unwrap(), &source_store)
        .unwrap();
    let targets = report
        .assets
        .iter()
        .map(|asset| asset.target_uri.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(targets.len(), 3, "{:?}", report.assets);
    // Only the two missing source assets stay unresolved.
    let issues = target_store.audit_assets(&appended);
    assert_eq!(
        issues
            .iter()
            .map(|issue| (issue.input_id, issue.reason))
            .collect::<Vec<_>>(),
        [
            (input_id(2), AssetAuditReason::MissingAsset),
            (input_id(4), AssetAuditReason::MissingAsset),
        ]
    );
    assert_eq!(
        fs::read(
            target_store
                .resolve_asset_uri(uri_of(&appended, 3))
                .unwrap()
        )
        .unwrap(),
        b"present source clip"
    );
}

#[test]
fn appending_a_project_into_itself_duplicates_with_fresh_ids() {
    let temp = TestDirectory::new("append-self");
    let store = temp.store("show.freemix");
    let project = stored(media_project(1, &[(1, "asset://a.mov")]));
    store.save(&project).unwrap();
    write_asset(&store, "a.mov", b"a");

    let (appended, report) = store.append_project(&project, &store).unwrap();
    assert_eq!(report.inputs, [(input_id(1), input_id(2))]);
    assert_eq!(appended.project().inputs()[1].name, "Input 1 (2)");
    assert_eq!(uri_of(&appended, 2), "asset://a.mov");
    assert_eq!(
        report.assets[0].disposition,
        AppendedAssetDisposition::Placed(AssetDisposition::Reused)
    );
    assert_eq!(tree(&store.assets_root()), ["a.mov"]);
}

#[test]
fn append_remaps_alias_targets_and_keeps_mix_feeds() {
    let temp = TestDirectory::new("append-alias");
    let store = temp.store("show.freemix");
    let mut project = media_project(1, &[(1, "asset://a.mov")]);
    project.add_input(Input {
        id: input_id(2),
        name: "Alias".into(),
        kind: InputKind::Alias {
            target: input_id(1),
            crop: Some(fm_model::CropRect::new(1, 2, 3, 4)),
        },
        required_capabilities: Vec::new(),
    });
    project.add_input(Input {
        id: input_id(3),
        name: "Program feed".into(),
        kind: InputKind::MixFeed(fm_model::MixFeed::Program),
        required_capabilities: Vec::new(),
    });
    let project = stored(project);
    store.save(&project).unwrap();
    write_asset(&store, "a.mov", b"a");

    let (appended, report) = store.append_project(&project, &store).unwrap();
    assert_eq!(
        report.inputs,
        [
            (input_id(1), input_id(4)),
            (input_id(2), input_id(5)),
            (input_id(3), input_id(6)),
        ]
    );
    let inputs = appended.project().inputs();
    assert_eq!(
        inputs[4].kind,
        InputKind::Alias {
            target: input_id(4),
            crop: Some(fm_model::CropRect::new(1, 2, 3, 4)),
        }
    );
    assert_eq!(
        inputs[5].kind,
        InputKind::MixFeed(fm_model::MixFeed::Program)
    );
    assert!(appended.project().validate().is_ok());
}

fn cube(scale: f32) -> String {
    let mut text = String::from("LUT_3D_SIZE 2\n");
    for blue in 0..2_u8 {
        for green in 0..2_u8 {
            for red in 0..2_u8 {
                let [red, green, blue] = [red, green, blue].map(|value| f32::from(value) * scale);
                text += &[red, green, blue].map(|value| value.to_string()).join(" ");
                text.push('\n');
            }
        }
    }
    text
}

fn graded_with_lut(lut: u32, exposure_millistops: i16) -> fm_grade::Grade {
    fm_grade::Grade {
        exposure_millistops,
        lut: Some(fm_grade::GradeLut::new(
            fm_model::lut_asset_uri(lut_id(lut)),
            500,
        )),
        ..fm_grade::Grade::IDENTITY
    }
}

fn lut_id(value: u32) -> fm_model::LutId {
    fm_model::LutId::new(std::num::NonZeroU32::new(value).unwrap())
}

fn grade_id(value: u32) -> fm_model::GradeId {
    fm_model::GradeId::new(std::num::NonZeroU32::new(value).unwrap())
}

fn film_lut(id: u32) -> fm_model::ProjectLut {
    fm_model::ProjectLut {
        id: lut_id(id),
        name: "Film".into(),
        kind: fm_grade::CubeKind::ThreeD,
        size: 2,
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn append_keeps_input_grades_and_imports_their_library_grades_and_luts() {
    use fm_model::{GradeSource, ProjectGrade, ProjectGrading};

    let temp = TestDirectory::new("append-grades");
    let target_store = temp.store("target.freemix");
    let source_store = temp.store("source.freemix");

    // Target: LUT 1 "Film" and library grade 1 "Warm" already exist.
    let mut target = media_project(1, &[(1, "asset://a.mov")]);
    let mut grading = ProjectGrading::default();
    grading.add_lut(film_lut(1)).unwrap();
    grading
        .insert_grade(
            None,
            ProjectGrade {
                id: grade_id(1),
                name: "Warm".into(),
                grade: graded_with_lut(1, 100),
            },
        )
        .unwrap();
    grading
        .set_input(input_id(1), Some(GradeSource::Library(grade_id(1))))
        .unwrap();
    target.set_grading(grading);
    let target = stored(target);
    target_store.save(&target).unwrap();
    write_asset(&target_store, "a.mov", b"a");
    target_store
        .write_lut_source(lut_id(1), &cube(1.0))
        .unwrap();

    // Source: the same ids and names with different contents; input 1 uses
    // library grade 1, input 2 an inline grade, both on LUT 1. An unused
    // library grade and LUT stay behind, as does the program grade.
    let mut source = media_project(2, &[(1, "asset://b.mov"), (2, "asset://c.mov")]);
    let mut grading = ProjectGrading::default();
    grading.add_lut(film_lut(1)).unwrap();
    grading
        .add_lut(fm_model::ProjectLut {
            name: "Unused".into(),
            ..film_lut(7)
        })
        .unwrap();
    grading
        .insert_grade(
            None,
            ProjectGrade {
                id: grade_id(1),
                name: "Warm".into(),
                grade: graded_with_lut(1, 200),
            },
        )
        .unwrap();
    grading
        .insert_grade(
            None,
            ProjectGrade {
                id: grade_id(5),
                name: "Spare".into(),
                grade: graded_with_lut(7, 0),
            },
        )
        .unwrap();
    grading
        .set_input(input_id(1), Some(GradeSource::Library(grade_id(1))))
        .unwrap();
    grading
        .set_input(
            input_id(2),
            Some(GradeSource::Inline(Box::new(graded_with_lut(1, 300)))),
        )
        .unwrap();
    grading
        .set_program(Some(GradeSource::Inline(Box::new(graded_with_lut(1, 0)))))
        .unwrap();
    source.set_grading(grading);
    let source = stored(source);
    source_store.save(&source).unwrap();
    write_asset(&source_store, "b.mov", b"b");
    write_asset(&source_store, "c.mov", b"c");
    source_store
        .write_lut_source(lut_id(1), &cube(0.5))
        .unwrap();
    source_store
        .write_lut_source(lut_id(7), &cube(0.25))
        .unwrap();

    let (appended, report) = target_store.append_project(&target, &source_store).unwrap();
    assert_eq!(
        report.inputs,
        [(input_id(1), input_id(2)), (input_id(2), input_id(3))]
    );
    assert_eq!(report.luts, [(lut_id(1), lut_id(2))]);
    assert_eq!(report.grades, [(grade_id(1), grade_id(2))]);
    assert!(report.skipped.program_grade);

    let grading = appended.project().grading();
    assert_eq!(
        grading
            .luts
            .iter()
            .map(|lut| (lut.id, lut.name.as_str()))
            .collect::<Vec<_>>(),
        [(lut_id(1), "Film"), (lut_id(2), "Film (2)")]
    );
    assert_eq!(grading.grades.len(), 2);
    assert_eq!(grading.grades[0].grade, graded_with_lut(1, 100));
    assert_eq!(grading.grades[1].id, grade_id(2));
    assert_eq!(grading.grades[1].name, "Warm (2)");
    assert_eq!(grading.grades[1].grade, graded_with_lut(2, 200));
    assert_eq!(
        grading.inputs.get(&input_id(1)),
        Some(&GradeSource::Library(grade_id(1)))
    );
    assert_eq!(
        grading.inputs.get(&input_id(2)),
        Some(&GradeSource::Library(grade_id(2)))
    );
    assert_eq!(
        grading.inputs.get(&input_id(3)),
        Some(&GradeSource::Inline(Box::new(graded_with_lut(2, 300))))
    );
    assert_eq!(grading.program, None);
    assert!(appended.project().validate().is_ok());

    // The source LUT's exact text now lives under its fresh id; the
    // target's own LUT is untouched.
    target_store.save(&appended).unwrap();
    let (copied, _) = target_store.load_lut_source(&grading.luts[1]).unwrap();
    assert_eq!(&*copied, cube(0.5));
    let (original, _) = target_store.load_lut_source(&grading.luts[0]).unwrap();
    assert_eq!(&*original, cube(1.0));
    assert!(!target_store.assets_root().join("luts/3.cube").exists());
}

// ---------------------------------------------------------------------------
// Media import
// ---------------------------------------------------------------------------

#[test]
fn media_import_dedupes_identical_files_and_never_overwrites() {
    let temp = TestDirectory::new("import");
    let store = temp.store("show.freemix");
    write(&temp.path("a/clip.mov"), b"first");
    write(&temp.path("b/clip.mov"), b"second");
    write(&temp.path("c/CON.mov"), b"device");
    write(&temp.path("d/what?.mov"), b"query");

    let first = store.import_media_file(&temp.path("a/clip.mov")).unwrap();
    assert_eq!(first.asset_uri, "asset://clip.mov");
    assert_eq!(first.disposition, AssetDisposition::Copied { bytes: 5 });
    let again = store.import_media_file(&temp.path("a/clip.mov")).unwrap();
    assert_eq!(again.asset_uri, "asset://clip.mov");
    assert_eq!(again.disposition, AssetDisposition::Reused);
    let second = store.import_media_file(&temp.path("b/clip.mov")).unwrap();
    assert_eq!(second.asset_uri, "asset://clip-2.mov");
    assert_eq!(
        store
            .import_media_file(&temp.path("c/CON.mov"))
            .unwrap()
            .asset_uri,
        "asset://_CON.mov"
    );
    assert_eq!(
        store
            .import_media_file(&temp.path("d/what?.mov"))
            .unwrap()
            .asset_uri,
        "asset://what_.mov"
    );
    assert_eq!(
        fs::read(store.resolve_asset_uri("asset://clip.mov").unwrap()).unwrap(),
        b"first"
    );
    assert_eq!(
        fs::read(store.resolve_asset_uri("asset://clip-2.mov").unwrap()).unwrap(),
        b"second"
    );
    assert!(matches!(
        store.import_media_file(&temp.path("a")),
        Err(BundleError::NotRegularFile(_))
    ));
    assert!(matches!(
        store.import_media_file(&temp.path("absent.mov")),
        Err(BundleError::Io(_))
    ));
    assert_eq!(
        tree(&store.assets_root()),
        ["_CON.mov", "clip-2.mov", "clip.mov", "what_.mov"]
    );
}

#[cfg(target_os = "linux")]
#[test]
fn media_import_converts_non_utf8_names_to_valid_keys() {
    use std::{ffi::OsStr, os::unix::ffi::OsStrExt as _};

    let temp = TestDirectory::new("import-non-utf8");
    let store = temp.store("show.freemix");
    let source = temp.0.join(OsStr::from_bytes(b"caf\xe9.mov"));
    fs::write(&source, b"bytes").unwrap();

    let imported = store.import_media_file(&source).unwrap();
    assert_eq!(imported.asset_uri, "asset://caf_.mov");
    assert_eq!(
        fs::read(store.resolve_asset_uri(&imported.asset_uri).unwrap()).unwrap(),
        b"bytes"
    );
}

// ---------------------------------------------------------------------------
// Missing-media relink
// ---------------------------------------------------------------------------

#[test]
#[allow(clippy::too_many_lines)] // one end-to-end relink scenario
fn relink_plan_is_deterministic_ranked_and_applies_only_unambiguous_matches() {
    let temp = TestDirectory::new("relink");
    let store = temp.store("show.freemix");
    let project = stored(media_project(
        1,
        &[
            (1, "asset://moved/a.mov"),
            (2, "asset://b.mov"),
            (3, "asset://c.mov"),
            (4, "asset://d.mov"),
            (5, "asset://none.mov"),
            (6, "asset://../e.mov"),
            (7, "asset://ok.mov"),
        ],
    ));
    store.save(&project).unwrap();
    write_asset(&store, "ok.mov", b"ok");
    write_asset(&store, "elsewhere/a.mov", b"a");
    let first = temp.path("first");
    let second = temp.path("second");
    write(&second.join("b.mov"), b"b-shallow-but-lower-priority");
    write(&first.join("deep/b.mov"), b"b");
    write(&first.join("x/c.mov"), b"c-one");
    write(&first.join("y/c.mov"), b"c-two");
    write(&first.join("p/d.mov"), b"d");
    write(&first.join("q/d.mov"), b"d");
    write(&first.join("e.mov"), b"e");

    let search = RelinkSearch::new(vec![first.clone(), second]);
    let plan = store.plan_media_relink(&project, &search).unwrap();
    assert_eq!(plan, store.plan_media_relink(&project, &search).unwrap());
    assert!(!plan.search_truncated);
    let summary = plan
        .entries
        .iter()
        .map(|entry| (entry.input_id, entry.reason, entry.resolution.clone()))
        .collect::<Vec<_>>();
    assert_eq!(
        summary,
        [
            (
                input_id(1),
                AssetAuditReason::MissingAsset,
                RelinkResolution::RewriteUri {
                    asset_uri: "asset://elsewhere/a.mov".into(),
                },
            ),
            (
                input_id(2),
                AssetAuditReason::MissingAsset,
                RelinkResolution::Import {
                    source: first.join("deep/b.mov"),
                    bytes: 1,
                },
            ),
            (
                input_id(3),
                AssetAuditReason::MissingAsset,
                RelinkResolution::Ambiguous {
                    candidates: vec![first.join("x/c.mov"), first.join("y/c.mov")],
                },
            ),
            (
                input_id(4),
                AssetAuditReason::MissingAsset,
                RelinkResolution::Import {
                    source: first.join("p/d.mov"),
                    bytes: 1,
                },
            ),
            (
                input_id(5),
                AssetAuditReason::MissingAsset,
                RelinkResolution::NotFound,
            ),
            (
                input_id(6),
                AssetAuditReason::InvalidUri,
                RelinkResolution::Unrelinkable,
            ),
        ]
    );

    let outcome = store.apply_media_relink(&project, &plan).unwrap();
    assert_eq!(outcome.relinked, [input_id(1), input_id(2), input_id(4)]);
    assert_eq!(outcome.unresolved, [input_id(3), input_id(5), input_id(6)]);
    assert_eq!(outcome.imported_bytes, 2);
    assert_eq!(uri_of(&outcome.project, 1), "asset://elsewhere/a.mov");
    assert_eq!(uri_of(&outcome.project, 2), "asset://b.mov");
    assert_eq!(
        fs::read(store.resolve_asset_uri("asset://b.mov").unwrap()).unwrap(),
        b"b"
    );
    store.save(&outcome.project).unwrap();
    let reopened = store.load().unwrap();
    assert_eq!(
        store
            .audit_assets(&reopened)
            .iter()
            .map(|issue| issue.input_id)
            .collect::<Vec<_>>(),
        [input_id(3), input_id(5), input_id(6)]
    );
    // `..` never lets a relink write outside the assets directory.
    assert!(!store.root().join("e.mov").exists());
}

#[test]
fn relink_apply_rejects_stale_plans_and_occupied_keys() {
    let temp = TestDirectory::new("relink-stale");
    let store = temp.store("show.freemix");
    let project = stored(media_project(1, &[(1, "asset://b.mov")]));
    store.save(&project).unwrap();
    write(&temp.path("search/b.mov"), b"found");
    let search = RelinkSearch::new(vec![temp.path("search")]);
    let plan = store.plan_media_relink(&project, &search).unwrap();

    let mut changed = project.project().clone();
    changed
        .relink_media_input(input_id(1), "asset://other.mov".into())
        .unwrap();
    let changed = project.with_project(changed).unwrap();
    assert!(matches!(
        store.apply_media_relink(&changed, &plan),
        Err(BundleError::StalePlan { input }) if input == input_id(1)
    ));

    write_asset(&store, "b.mov", b"different");
    assert!(matches!(
        store.apply_media_relink(&project, &plan),
        Err(BundleError::Collision { key }) if key == "b.mov"
    ));
    assert_eq!(
        fs::read(store.assets_root().join("b.mov")).unwrap(),
        b"different"
    );

    // An identical occupant is accepted as already relinked.
    fs::write(store.assets_root().join("b.mov"), b"found").unwrap();
    let outcome = store.apply_media_relink(&project, &plan).unwrap();
    assert_eq!(outcome.relinked, [input_id(1)]);
    assert_eq!(outcome.imported_bytes, 0);

    // A source that changed size since planning is stale.
    fs::remove_file(store.assets_root().join("b.mov")).unwrap();
    fs::write(temp.path("search/b.mov"), b"grown since the plan").unwrap();
    assert!(matches!(
        store.apply_media_relink(&project, &plan),
        Err(BundleError::StalePlan { .. })
    ));
}

#[test]
fn relink_search_is_bounded() {
    let temp = TestDirectory::new("relink-bounded");
    let store = temp.store("show.freemix");
    let project = stored(media_project(1, &[(1, "asset://deep.mov")]));
    store.save(&project).unwrap();
    write(&temp.path("search/1/2/3/deep.mov"), b"deep");

    let mut search = RelinkSearch::new(vec![temp.path("search")]);
    search.max_depth = 2;
    let plan = store.plan_media_relink(&project, &search).unwrap();
    assert_eq!(plan.entries[0].resolution, RelinkResolution::NotFound);

    search.max_depth = 3;
    let plan = store.plan_media_relink(&project, &search).unwrap();
    assert!(matches!(
        plan.entries[0].resolution,
        RelinkResolution::Import { .. }
    ));

    search.max_entries = 2;
    let plan = store.plan_media_relink(&project, &search).unwrap();
    assert!(plan.search_truncated);
    assert_eq!(plan.entries[0].resolution, RelinkResolution::NotFound);

    assert!(matches!(
        store.plan_media_relink(&project, &RelinkSearch::new(vec![temp.path("absent")])),
        Err(BundleError::Io(_))
    ));
}

#[cfg(target_os = "linux")]
#[test]
fn relink_search_ignores_symlinked_directories_and_tolerates_non_utf8_names() {
    use std::{ffi::OsStr, os::unix::ffi::OsStrExt as _, os::unix::fs::symlink};

    let temp = TestDirectory::new("relink-links");
    let store = temp.store("show.freemix");
    let project = stored(media_project(
        1,
        &[(1, "asset://hidden.mov"), (2, "asset://linked.mov")],
    ));
    store.save(&project).unwrap();
    let search_root = temp.path("search");
    fs::create_dir(&search_root).unwrap();
    // A directory loop and a symlinked directory are not descended into.
    symlink(&search_root, search_root.join("loop")).unwrap();
    write(&temp.path("elsewhere/hidden.mov"), b"hidden");
    symlink(temp.path("elsewhere"), search_root.join("dir-link")).unwrap();
    // A symlinked file is a candidate; its target's bytes are imported.
    write(&temp.path("real.mov"), b"linked");
    symlink(temp.path("real.mov"), search_root.join("linked.mov")).unwrap();
    // A non-UTF-8 name neither matches nor breaks the search.
    fs::write(search_root.join(OsStr::from_bytes(b"linked\xff.mov")), b"x").unwrap();

    let plan = store
        .plan_media_relink(&project, &RelinkSearch::new(vec![search_root.clone()]))
        .unwrap();
    assert_eq!(plan.entries[0].resolution, RelinkResolution::NotFound);
    assert_eq!(
        plan.entries[1].resolution,
        RelinkResolution::Import {
            source: search_root.join("linked.mov"),
            bytes: 6,
        }
    );
    let outcome = store.apply_media_relink(&project, &plan).unwrap();
    let imported = store.assets_root().join("linked.mov");
    assert!(fs::symlink_metadata(&imported).unwrap().is_file());
    assert_eq!(fs::read(imported).unwrap(), b"linked");
    assert_eq!(outcome.relinked, [input_id(2)]);
}

#[cfg(unix)]
#[test]
fn relink_import_refuses_a_key_directory_that_escapes_the_bundle() {
    let temp = TestDirectory::new("relink-escape");
    let store = temp.store("show.freemix");
    let project = stored(media_project(1, &[(1, "asset://sub/x.mov")]));
    store.save(&project).unwrap();
    fs::create_dir_all(store.assets_root()).unwrap();
    fs::create_dir(temp.path("outside")).unwrap();
    std::os::unix::fs::symlink(temp.path("outside"), store.assets_root().join("sub")).unwrap();
    write(&temp.path("search/x.mov"), b"x");

    let plan = store
        .plan_media_relink(&project, &RelinkSearch::new(vec![temp.path("search")]))
        .unwrap();
    assert!(matches!(
        plan.entries[0].resolution,
        RelinkResolution::Import { .. }
    ));
    assert!(matches!(
        store.apply_media_relink(&project, &plan),
        Err(BundleError::Io(error)) if error.kind() == std::io::ErrorKind::InvalidInput
    ));
    assert_eq!(tree(&temp.path("outside")), Vec::<String>::new());
}

#[cfg(unix)]
#[test]
fn relink_never_proposes_an_in_bundle_symlink_that_escapes_the_assets() {
    let temp = TestDirectory::new("relink-escaping-file-link");
    let store = temp.store("show.freemix");
    let project = stored(media_project(
        1,
        &[(1, "asset://clip.mov"), (2, "asset://other.mov")],
    ));
    store.save(&project).unwrap();
    write(&temp.path("outside/clip.mov"), b"outside");
    fs::create_dir_all(store.assets_root().join("links")).unwrap();
    std::os::unix::fs::symlink(
        temp.path("outside/clip.mov"),
        store.assets_root().join("links/clip.mov"),
    )
    .unwrap();
    write(&temp.path("search/other.mov"), b"other");

    let plan = store
        .plan_media_relink(&project, &RelinkSearch::new(vec![temp.path("search")]))
        .unwrap();
    assert_eq!(plan.entries[0].resolution, RelinkResolution::NotFound);
    // The unusable link does not block the other repair.
    let outcome = store.apply_media_relink(&project, &plan).unwrap();
    assert_eq!(outcome.relinked, [input_id(2)]);
    assert_eq!(outcome.unresolved, [input_id(1)]);
}

/// A process-unique layer id, so layers built by helpers never collide.
fn next_layer_id() -> fm_model::LayerId {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);
    fm_model::LayerId::new(NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
        .expect("layer ids start at one")
}

#[test]
fn append_keeps_scene_layer_grades_under_the_fresh_scene_ids() {
    use fm_model::{GradeSource, ProjectGrade, ProjectGrading, SceneLayerRef};

    let temp = TestDirectory::new("append-layer-grades");
    let target_store = temp.store("target.freemix");
    let source_store = temp.store("source.freemix");
    let mut target = media_project(1, &[(1, "asset://a.mov")]);
    target
        .add_scene_input_checked(scene_id(1), "Main".into(), input_id(2), "Main".into())
        .unwrap();
    let target = stored(target);
    target_store.save(&target).unwrap();
    write_asset(&target_store, "a.mov", b"a");

    // Source: scene 4 with two layers on input 1; layer one carries library
    // grade 3, layer two an inline grade on LUT 6.
    let mut source = media_project(2, &[(1, "asset://b.mov")]);
    let host = layer("host", SourceRef::Input(input_id(1)));
    let inset = layer("inset", SourceRef::Input(input_id(1)));
    let (host_id, inset_id) = (host.id, inset.id);
    source.add_scene(Scene {
        id: scene_id(4),
        name: "Two up".into(),
        background: Rgba8::OPAQUE_BLACK,
        layers: vec![host, inset],
        animation: fm_model::SceneAnimation::default(),
    });
    let mut grading = ProjectGrading::default();
    grading.add_lut(film_lut(6)).unwrap();
    grading
        .insert_grade(
            None,
            ProjectGrade {
                id: grade_id(3),
                name: "Night".into(),
                grade: graded_with_lut(6, -100),
            },
        )
        .unwrap();
    grading
        .set_layer(
            SceneLayerRef::new(scene_id(4), host_id),
            Some(GradeSource::Library(grade_id(3))),
        )
        .unwrap();
    grading
        .set_layer(
            SceneLayerRef::new(scene_id(4), inset_id),
            Some(GradeSource::Inline(Box::new(graded_with_lut(6, 400)))),
        )
        .unwrap();
    source.set_grading(grading);
    source_store.save(&stored(source)).unwrap();
    write_asset(&source_store, "b.mov", b"b");
    source_store
        .write_lut_source(lut_id(6), &cube(0.5))
        .unwrap();

    let (appended, report) = target_store.append_project(&target, &source_store).unwrap();
    assert_eq!(report.scenes, [(scene_id(4), scene_id(2))]);
    assert_eq!(report.luts, [(lut_id(6), lut_id(1))]);
    assert_eq!(report.grades, [(grade_id(3), grade_id(1))]);
    let grading = appended.project().grading();
    assert_eq!(
        grading
            .layers
            .get(&SceneLayerRef::new(scene_id(2), host_id)),
        Some(&GradeSource::Library(grade_id(1)))
    );
    assert_eq!(
        grading
            .layers
            .get(&SceneLayerRef::new(scene_id(2), inset_id)),
        Some(&GradeSource::Inline(Box::new(graded_with_lut(1, 400))))
    );
    assert_eq!(grading.grades[0].grade, graded_with_lut(1, -100));
    assert!(appended.project().validate().is_ok());
}

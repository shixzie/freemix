//! RC-012 acceptance: rotating autosave generations with recovery, and the
//! persisted recent-projects list.

use std::{
    fs,
    num::NonZeroU128,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use fm_model::{Input, InputKind, Project, ProjectSettings};
use fm_persistence::{
    AutosaveError, AutosaveRejection, MAX_AUTOSAVE_GENERATIONS, MAX_RECENT_PROJECTS,
    ProjectPosition, ProjectStore, RecentProject, RecentProjectStatus, RecentProjects,
    RecentProjectsError, RuntimeRouting, StoreError, StoredProject,
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
            "fm-persistence-autosave-recent-{}-{sequence}-{name}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }

    fn store(&self, name: &str) -> ProjectStore {
        ProjectStore::new(self.0.join(name)).unwrap()
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn project(id: u128, revision: u64, inputs: u128) -> StoredProject {
    let frame_rate = FrameRate::new(25, 1).unwrap();
    let mut project = Project::new(
        ProjectId::new(NonZeroU128::new(id).unwrap()),
        format!("Show {id}"),
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
    );
    for input in 1..=inputs {
        project.add_input(Input {
            id: InputId::new(NonZeroU128::new(input).unwrap()),
            name: format!("Input {input}"),
            kind: InputKind::Color,
            required_capabilities: Vec::new(),
        });
    }
    StoredProject::from_project(
        project,
        RuntimeRouting::default(),
        ProjectPosition {
            revision,
            ..ProjectPosition::default()
        },
        Vec::new(),
    )
    .unwrap()
}

fn file_names(directory: &Path) -> Vec<String> {
    let mut names = fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    names.sort();
    names
}

// ---------------------------------------------------------------------------
// Autosave
// ---------------------------------------------------------------------------

#[test]
fn autosave_rotation_keeps_the_newest_generations() {
    let temp = TestDirectory::new("rotation");
    let store = temp.store("show.freemix");
    store.save(&project(1, 0, 0)).unwrap();

    let mut pruned = Vec::new();
    for revision in 1..=7 {
        let write = store
            .write_autosave(&project(1, revision, u128::from(revision)), 3)
            .unwrap();
        assert_eq!(write.generation, revision);
        pruned.extend(write.pruned);
    }
    assert_eq!(pruned, [1, 2, 3, 4]);
    assert_eq!(
        file_names(&store.autosave_path()),
        [
            "autosave-00000000000000000005.json",
            "autosave-00000000000000000006.json",
            "autosave-00000000000000000007.json",
        ]
    );
    // Each generation is a complete manifest in the current format.
    assert_eq!(
        fs::read(
            store
                .autosave_path()
                .join("autosave-00000000000000000007.json")
        )
        .unwrap(),
        {
            let other = temp.store("other.freemix");
            other.save(&project(1, 7, 7)).unwrap();
            fs::read(other.manifest_path()).unwrap()
        }
    );

    // Lowering the bound prunes down to it on the next write.
    let write = store.write_autosave(&project(1, 8, 8), 1).unwrap();
    assert_eq!(write.generation, 8);
    assert_eq!(write.pruned, [5, 6, 7]);
    assert_eq!(store.list_autosaves().unwrap().len(), 1);

    let recovery = store.recover_autosave().unwrap();
    assert!(recovery.rejected.is_empty());
    assert!(recovery.differs_from_manifest);
    let (generation, recovered) = recovery.recovered.unwrap();
    assert_eq!(generation, 8);
    assert_eq!(recovered, project(1, 8, 8));
    // Recovery never rewrites the manifest by itself.
    assert_eq!(store.load().unwrap(), project(1, 0, 0));
}

#[test]
fn autosave_recovery_skips_damaged_and_foreign_generations() {
    let temp = TestDirectory::new("recovery");
    let store = temp.store("show.freemix");
    store.save(&project(1, 2, 2)).unwrap();
    store.write_autosave(&project(1, 2, 2), 5).unwrap();
    store.write_autosave(&project(1, 3, 3), 5).unwrap();
    store.write_autosave(&project(1, 4, 4), 5).unwrap();
    store.write_autosave(&project(99, 5, 5), 5).unwrap();
    store.write_autosave(&project(1, 6, 6), 5).unwrap();

    // Generation 5 is torn mid-write and generation 3 holds a structurally
    // incomplete document.
    let path_of = |generation: u64| {
        store
            .autosave_path()
            .join(format!("autosave-{generation:020}.json"))
    };
    let torn = fs::read(path_of(5)).unwrap();
    fs::write(path_of(5), &torn[..torn.len() / 2]).unwrap();
    fs::write(path_of(3), b"{}").unwrap();
    // Stray files, including an interrupted temporary, are ignored.
    fs::write(
        store
            .autosave_path()
            .join(".autosave-00000000000000000009.json.tmp-1-1"),
        b"partial",
    )
    .unwrap();
    fs::write(store.autosave_path().join("notes.txt"), b"x").unwrap();

    let recovery = store.recover_autosave().unwrap();
    let rejected = recovery
        .rejected
        .iter()
        .map(|(generation, _)| *generation)
        .collect::<Vec<_>>();
    assert_eq!(rejected, [5, 4, 3]);
    assert!(matches!(
        recovery.rejected[0].1,
        AutosaveRejection::Unreadable(StoreError::MalformedManifest { .. })
    ));
    assert!(matches!(
        recovery.rejected[1].1,
        AutosaveRejection::ForeignProject { found, expected }
            if found.get().get() == 99 && expected.get().get() == 1
    ));
    assert!(matches!(
        recovery.rejected[2].1,
        AutosaveRejection::Unreadable(_)
    ));
    let (generation, recovered) = recovery.recovered.unwrap();
    assert_eq!(generation, 2);
    assert_eq!(recovered, project(1, 3, 3));
    assert!(recovery.differs_from_manifest);

    // The next generation continues above every existing name.
    assert_eq!(
        store
            .write_autosave(&project(1, 7, 7), 5)
            .unwrap()
            .generation,
        6
    );
}

#[test]
fn autosave_recovery_without_a_usable_manifest_accepts_any_valid_snapshot() {
    let temp = TestDirectory::new("no-manifest");
    let store = temp.store("show.freemix");
    let empty = store.recover_autosave().unwrap();
    assert!(empty.recovered.is_none());
    assert!(empty.rejected.is_empty());

    store.write_autosave(&project(4, 1, 1), 2).unwrap();
    let recovery = store.recover_autosave().unwrap();
    assert_eq!(recovery.recovered.unwrap().1, project(4, 1, 1));
    assert!(recovery.differs_from_manifest);

    // An identical manifest is reported as such.
    store.save(&project(4, 1, 1)).unwrap();
    assert!(!store.recover_autosave().unwrap().differs_from_manifest);

    // A corrupt manifest does not block recovery.
    fs::write(store.manifest_path(), b"{").unwrap();
    let recovery = store.recover_autosave().unwrap();
    assert!(recovery.recovered.is_some());
    assert!(recovery.differs_from_manifest);
}

#[test]
fn autosave_rejects_out_of_range_retention_without_writing() {
    let temp = TestDirectory::new("retention");
    let store = temp.store("show.freemix");
    for retain in [0, MAX_AUTOSAVE_GENERATIONS + 1] {
        assert!(matches!(
            store.write_autosave(&project(1, 0, 0), retain),
            Err(AutosaveError::InvalidRetention { requested }) if requested == retain
        ));
    }
    assert!(!store.autosave_path().exists());
    assert_eq!(store.list_autosaves().unwrap(), Vec::new());
}

// ---------------------------------------------------------------------------
// Recent projects
// ---------------------------------------------------------------------------

#[test]
fn recent_projects_dedupe_by_canonical_path_and_stay_bounded() {
    let temp = TestDirectory::new("recent");
    let stores = (1..=4)
        .map(|index| {
            let store = temp.store(&format!("show{index}.freemix"));
            store.save(&project(index, 0, 0)).unwrap();
            store
        })
        .collect::<Vec<_>>();
    let mut recent = RecentProjects::new(3).unwrap();
    for (index, store) in stores.iter().enumerate() {
        recent
            .record(store, &format!("Show {}", index + 1), 100 + index as u64)
            .unwrap();
    }
    let names = |recent: &RecentProjects| {
        recent
            .entries()
            .iter()
            .map(|entry| entry.name.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(names(&recent), ["Show 4", "Show 3", "Show 2"]);

    // The same bundle reached through `..` moves to the front, not a duplicate.
    let indirect = ProjectStore::new(temp.0.join("show4.freemix/../show2.freemix")).unwrap();
    recent.record(&indirect, "Show 2 again", 200).unwrap();
    assert_eq!(names(&recent), ["Show 2 again", "Show 4", "Show 3"]);
    assert_eq!(recent.entries()[0].path, stores[1].root());
    assert_eq!(recent.entries()[0].opened_at_unix_ms, 200);
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(stores[2].root(), temp.0.join("alias.freemix")).unwrap();
        let alias = ProjectStore::new(temp.0.join("alias.freemix")).unwrap();
        recent.record(&alias, "Show 3 via alias", 300).unwrap();
        assert_eq!(
            names(&recent),
            ["Show 3 via alias", "Show 2 again", "Show 4"]
        );
        assert_eq!(recent.entries()[0].path, stores[2].root());
    }

    // A bundle that does not exist cannot be recorded; the list is unchanged.
    let before = recent.clone();
    assert!(matches!(
        recent.record(&temp.store("absent.freemix"), "Absent", 1),
        Err(RecentProjectsError::Io(_))
    ));
    assert_eq!(recent, before);

    let file = temp.0.join("config/recent-projects.json");
    recent.save(&file).unwrap();
    assert_eq!(file_names(file.parent().unwrap()), ["recent-projects.json"]);
    assert_eq!(RecentProjects::load(&file, 3).unwrap(), recent);
    // Loading with a smaller bound keeps the most recent entries.
    let shorter = RecentProjects::load(&file, 1).unwrap();
    assert_eq!(shorter.entries(), &recent.entries()[..1]);
    // A missing file is an empty list.
    assert_eq!(
        RecentProjects::load(&temp.0.join("none.json"), 3).unwrap(),
        RecentProjects::new(3).unwrap()
    );

    assert!(recent.remove(stores[3].root()));
    assert!(!recent.remove(stores[3].root()));
}

#[test]
fn recent_projects_flag_and_prune_unavailable_bundles() {
    let temp = TestDirectory::new("recent-status");
    let available = temp.store("available.freemix");
    let missing = temp.store("missing.freemix");
    let hollow = temp.store("hollow.freemix");
    for store in [&available, &missing, &hollow] {
        store.save(&project(1, 0, 0)).unwrap();
    }
    let mut recent = RecentProjects::new(10).unwrap();
    recent.record(&available, "Available", 1).unwrap();
    recent.record(&missing, "Missing", 2).unwrap();
    recent.record(&hollow, "Hollow", 3).unwrap();
    fs::remove_dir_all(missing.root()).unwrap();
    fs::remove_file(hollow.manifest_path()).unwrap();

    let statuses = recent
        .statuses()
        .into_iter()
        .map(|(entry, status)| (entry.name.as_str(), status))
        .collect::<Vec<_>>();
    assert_eq!(
        statuses,
        [
            ("Hollow", RecentProjectStatus::NotABundle),
            ("Missing", RecentProjectStatus::Missing),
            ("Available", RecentProjectStatus::Available),
        ]
    );
    // A missing entry stays listed (and persisted) until pruned.
    let file = temp.0.join("recent.json");
    recent.save(&file).unwrap();
    assert_eq!(RecentProjects::load(&file, 10).unwrap().entries().len(), 3);

    let removed = recent.prune_unavailable();
    assert_eq!(
        removed
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        ["Hollow", "Missing"]
    );
    assert_eq!(recent.entries().len(), 1);
    assert_eq!(recent.entries()[0].name, "Available");
}

#[test]
fn recent_projects_document_is_strict() {
    let temp = TestDirectory::new("recent-strict");
    let file = temp.0.join("recent.json");
    let load = |document: &str| {
        fs::write(&file, document).unwrap();
        RecentProjects::load(&file, 10)
    };
    let entry = |extra: &str| {
        format!(
            r#"{{"contract":"freemix.recent-projects","entries":[{{"path":"/a/show.freemix","name":"A","opened_at_unix_ms":1{extra}}}]}}"#
        )
    };

    let list = load(&entry("")).unwrap();
    assert_eq!(
        list.entries(),
        [RecentProject {
            path: PathBuf::from("/a/show.freemix"),
            name: "A".into(),
            opened_at_unix_ms: 1,
        }]
    );
    assert!(matches!(
        load(&entry(r#","pinned":true"#)),
        Err(RecentProjectsError::Shape(_))
    ));
    assert!(matches!(
        load(&entry(r#","path_hex":"ff""#)),
        Err(RecentProjectsError::Shape(_))
    ));
    assert!(matches!(
        load(r#"{"contract":"freemix.recent-projects/2","entries":[]}"#),
        Err(RecentProjectsError::ContractMismatch)
    ));
    assert!(matches!(
        load(r#"{"contract":"freemix.recent-projects","entries":[],"extra":1}"#),
        Err(RecentProjectsError::Shape(_))
    ));
    assert!(matches!(
        load(
            r#"{"contract":"freemix.recent-projects","entries":[{"path":"relative.freemix","name":"A","opened_at_unix_ms":1}]}"#
        ),
        Err(RecentProjectsError::Shape(_))
    ));
    assert!(matches!(
        load(
            r#"{"contract":"freemix.recent-projects","entries":[{"path":"/a.freemix","name":"A","opened_at_unix_ms":1},{"path":"/a.freemix","name":"B","opened_at_unix_ms":2}]}"#
        ),
        Err(RecentProjectsError::DuplicatePath(_))
    ));
    // A UTF-8 path must use `path`, never `path_hex`.
    assert!(matches!(
        load(
            r#"{"contract":"freemix.recent-projects","entries":[{"path_hex":"2f61","name":"A","opened_at_unix_ms":1}]}"#
        ),
        Err(RecentProjectsError::Shape(_))
    ));
    assert!(matches!(
        load(r#"{"contract":"freemix.recent-projects","entries":[]"#),
        Err(RecentProjectsError::Syntax { .. })
    ));
    assert!(matches!(
        load(
            r#"{"contract":"freemix.recent-projects","entries":[{"path":"/a.freemix","name":"A","opened_at_unix_ms":-1}]}"#
        ),
        Err(RecentProjectsError::Shape(_))
    ));
    for capacity in [0, MAX_RECENT_PROJECTS + 1] {
        assert!(matches!(
            RecentProjects::new(capacity),
            Err(RecentProjectsError::InvalidCapacity { .. })
        ));
    }
}

#[cfg(target_os = "linux")]
#[test]
fn recent_projects_round_trip_non_utf8_paths() {
    use std::{ffi::OsStr, os::unix::ffi::OsStrExt as _};

    let temp = TestDirectory::new("recent-non-utf8");
    let store = ProjectStore::new(temp.0.join(OsStr::from_bytes(b"caf\xe9.freemix"))).unwrap();
    store.save(&project(1, 0, 0)).unwrap();
    let mut recent = RecentProjects::new(5).unwrap();
    recent.record(&store, "Caf\u{e9}", 5).unwrap();

    let file = temp.0.join("recent.json");
    recent.save(&file).unwrap();
    let document = fs::read_to_string(&file).unwrap();
    assert!(document.contains("\"path_hex\":\""), "{document}");
    assert!(document.contains("636166e92e667265656d6978"), "{document}");
    let loaded = RecentProjects::load(&file, 5).unwrap();
    assert_eq!(loaded, recent);
    assert_eq!(loaded.statuses()[0].1, RecentProjectStatus::Available);
}

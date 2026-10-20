//! IN-006: media lists round-trip exactly and strictly in `project.json`;
//! bounds, the active item, and whether its input plays it are project
//! validation.

use std::{
    collections::BTreeMap,
    fs,
    num::NonZeroU128,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use fm_model::{
    Input, InputKind, MediaList, MediaListItem, MediaListOptions, MediaPlaybackSettings, Project,
    ProjectSettings,
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
            "fm-persistence-media-lists-{}-{sequence}-{name}",
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

fn project() -> Project {
    let frame_rate = FrameRate::new(30, 1).unwrap();
    let mut project = Project::new(
        ProjectId::new(NonZeroU128::new(1).unwrap()),
        "Lists",
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
    project.add_input(Input {
        id: input(7),
        name: "Walk-in".into(),
        kind: InputKind::Media {
            asset_uri: "asset://media/b.wav".into(),
            playback: MediaPlaybackSettings::default(),
        },
        required_capabilities: Vec::new(),
    });
    let list = MediaList {
        items: vec![
            MediaListItem {
                id: 1,
                asset_uri: "asset://media/a.mp4".into(),
                title: Some("Intro \"cold\"".into()),
                mark_in: 12,
                mark_out: Some(250),
            },
            MediaListItem::new(4, "asset://media/b.wav"),
        ],
        options: MediaListOptions {
            auto_next: true,
            auto_first: false,
            loop_list: true,
            shuffle_seed: Some(18_446_744_073_709_551_615),
        },
        active: 4,
        cycle: 3,
    };
    project.set_media_lists(BTreeMap::from([(input(7), list)]));
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
fn media_lists_round_trip_exactly_and_strictly() {
    let directory = TestDirectory::new("round-trip");
    let store = ProjectStore::new(directory.path("show.freemix")).unwrap();
    let expected = project();
    store.save(&stored(expected.clone()).unwrap()).unwrap();
    assert_eq!(store.load().unwrap().project(), &expected);

    let text = fs::read_to_string(directory.path("show.freemix/project.json")).unwrap();
    assert!(
        text.contains(
            ",\n    \"media_lists\": [\n      {\"input\": 7, \"active\": 4, \"cycle\": 3, \
             \"auto_next\": true, \"auto_first\": false, \"loop\": true, \"shuffle_seed\": \
             18446744073709551615, \"items\": [{\"id\": 1, \"uri\": \"asset://media/a.mp4\", \
             \"title\": \"Intro \\\"cold\\\"\", \"mark_in\": 12, \"mark_out\": 250}, {\"id\": 4, \
             \"uri\": \"asset://media/b.wav\", \"title\": null, \"mark_in\": 0, \"mark_out\": \
             null}]}\n    ],\n    \"title_timelines\": []\n  },"
        ),
        "{text}"
    );

    for (from, to) in [
        ("\"media_lists\": [", "\"media_list\": ["),
        ("\"active\": 4", "\"active\": \"4\""),
        ("\"loop\": true", "\"looping\": true"),
        (
            "\"shuffle_seed\": 18446744073709551615",
            "\"shuffle_seed\": 18446744073709551616",
        ),
        ("\"auto_first\": false, ", ""),
        ("\"title\": null", "\"title\": 3"),
        (
            "\"mark_out\": null}",
            "\"mark_out\": null, \"duration\": 5}",
        ),
        ("{\"id\": 4, ", "{"),
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

    // Bounds, the active item, and the input's file are project validation.
    for (from, to) in [
        ("\"active\": 4", "\"active\": 9"),
        ("\"active\": 4", "\"active\": 1"),
        ("{\"id\": 4, ", "{\"id\": 1, "),
        (
            "\"mark_in\": 12, \"mark_out\": 250",
            "\"mark_in\": 12, \"mark_out\": 12",
        ),
        ("asset://media/a.mp4", "/home/show/a.mp4"),
        ("asset://media/a.mp4", "asset://media/a.png"),
        ("\"input\": 7, \"active\"", "\"input\": 8, \"active\""),
        (
            "\"items\": [{\"id\": 1",
            "\"items\": [], \"x\": [{\"id\": 1",
        ),
    ] {
        let invalid = text.replacen(from, to, 1);
        assert_ne!(invalid, text, "{from}");
        fs::write(directory.path("show.freemix/project.json"), invalid).unwrap();
        let loaded = store.load();
        assert!(
            matches!(
                loaded,
                Err(StoreError::Validation(_) | StoreError::MalformedManifest { .. })
            ),
            "{from} -> {to}: {loaded:?}"
        );
    }
}

#[test]
fn a_list_whose_input_plays_another_file_is_refused() {
    let mut project = project();
    let mut lists = project.media_lists().clone();
    lists.get_mut(&input(7)).unwrap().active = 1;
    project.set_media_lists(lists);
    assert!(stored(project).is_err());
}

#[test]
fn a_portable_bundle_carries_every_file_of_a_list() {
    let directory = TestDirectory::new("export");
    let source = ProjectStore::new(directory.path("show.freemix")).unwrap();
    let expected = project();
    source.save(&stored(expected.clone()).unwrap()).unwrap();
    for key in ["media/a.mp4", "media/b.wav"] {
        let path = source.assets_root().join(key);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, key.as_bytes()).unwrap();
    }
    let destination = ProjectStore::new(directory.path("copy.freemix")).unwrap();
    let export = source.export_portable_bundle(&destination).unwrap();
    let mut bundled: Vec<&str> = export
        .assets
        .iter()
        .map(|asset| asset.bundled_uri.as_str())
        .collect();
    bundled.sort_unstable();
    assert_eq!(bundled, ["asset://media/a.mp4", "asset://media/b.wav"]);
    assert_eq!(
        destination.load().unwrap().project().media_lists(),
        expected.media_lists()
    );
    assert!(destination.assets_root().join("media/a.mp4").is_file());
}

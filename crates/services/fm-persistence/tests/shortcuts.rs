//! RC-004: the project's user shortcuts round-trip exactly and strictly in
//! `project.json` (a key's chord stroke and panel scope since schema 70);
//! bounds, duplicate ids, clashing keys, duplicate OSC names, and waits in
//! an action list are project validation.

use std::{
    fs,
    num::NonZeroU128,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use fm_model::{
    CueAction, MAX_SHORTCUTS, Project, ProjectSettings, Shortcut, ShortcutAction,
    ShortcutCueOperation, ShortcutKey,
};
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
            "fm-persistence-shortcuts-{}-{sequence}-{name}",
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
        "Shortcuts",
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

fn input(value: u128) -> fm_types::InputId {
    fm_types::InputId::new(NonZeroU128::new(value).unwrap())
}

fn shortcuts() -> Vec<Shortcut> {
    vec![
        Shortcut {
            id: 4,
            name: "Intro \"lower\"".into(),
            enabled: true,
            key: Some(ShortcutKey {
                key: "F9".into(),
                command: false,
                shift: true,
                alt: false,
                then: None,
                scope: None,
            }),
            osc: Some("intro".into()),
            action: ShortcutAction::Actions(vec![
                CueAction::OverlayOn {
                    channel: 1,
                    input: input(4),
                },
                CueAction::AudioMute {
                    input: input(2),
                    muted: true,
                },
            ]),
        },
        Shortcut {
            id: 2,
            name: "Open".into(),
            enabled: true,
            key: None,
            osc: None,
            action: ShortcutAction::Cue {
                list: 1,
                operation: ShortcutCueOperation::Go { cue: None },
            },
        },
        Shortcut {
            id: 3,
            name: "Scores".into(),
            enabled: false,
            key: None,
            osc: Some("scores".into()),
            action: ShortcutAction::Script { script: 2 },
        },
        Shortcut {
            id: 5,
            name: "Back".into(),
            enabled: true,
            // Ctrl+K then 1, in the Cues panel.
            key: Some(ShortcutKey {
                key: "K".into(),
                command: true,
                shift: false,
                alt: false,
                then: Some(Box::new(ShortcutKey::plain("1"))),
                scope: Some("cues".into()),
            }),
            osc: None,
            action: ShortcutAction::Cue {
                list: 1,
                operation: ShortcutCueOperation::Back,
            },
        },
    ]
}

#[test]
fn shortcuts_round_trip_exactly_and_strictly() {
    let directory = TestDirectory::new("round-trip");
    let store = ProjectStore::new(directory.path("show.freemix")).unwrap();
    let mut expected = project();
    expected.set_shortcuts(shortcuts());
    store.save(&stored(expected.clone()).unwrap()).unwrap();
    assert_eq!(store.load().unwrap().project(), &expected);

    let text = fs::read_to_string(directory.path("show.freemix/project.json")).unwrap();
    assert!(
        text.contains(
            ",\n    \"shortcuts\": [\n      {\"id\": 4, \"name\": \"Intro \\\"lower\\\"\", \
             \"enabled\": true, \"key\": {\"key\": \"F9\", \"command\": false, \"shift\": true, \
             \"alt\": false, \"then\": null, \"scope\": null}, \"osc\": \"intro\", \"action\": {\"type\": \"actions\", \
             \"actions\": [{\"type\": \"overlay_on\", \"channel\": 1, \"input\": 4}, {\"type\": \
             \"audio_mute\", \"input\": 2, \"muted\": true}]}},\n      {\"id\": 2, \"name\": \
             \"Open\", \"enabled\": true, \"key\": null, \"osc\": null, \"action\": {\"type\": \
             \"cue\", \"list\": 1, \"operation\": \"go\", \"cue\": null}},\n      {\"id\": 3, \
             \"name\": \"Scores\", \"enabled\": false, \"key\": null, \"osc\": \"scores\", \
             \"action\": {\"type\": \"script\", \"script\": 2}},\n      {\"id\": 5, \"name\": \
             \"Back\", \"enabled\": true, \"key\": {\"key\": \"K\", \"command\": true, \
             \"shift\": false, \"alt\": false, \"then\": {\"key\": \"1\", \"command\": false, \
             \"shift\": false, \"alt\": false}, \"scope\": \"cues\"}, \"osc\": null, \"action\": {\"type\": \"cue\", \
             \"list\": 1, \"operation\": \"back\"}}\n    ],\n    \"media_lists\": [],\n    \"title_timelines\": [],\n    \"screen_capture_restore_tokens\": []\n  },"
        ),
        "{text}"
    );

    // Malformed JSON is a malformed manifest.
    for (from, to) in [
        ("\"shortcuts\": [", "\"shortcut\": ["),
        ("{\"id\": 2, \"name\"", "{\"id\": \"2\", \"name\""),
        ("\"enabled\": false", "\"enabled\": 0"),
        ("\"osc\": \"scores\"", "\"osc\": 7"),
        ("\"command\": false, \"shift\": true", "\"shift\": true"),
        (
            "\"scope\": null}, \"osc\": \"intro\"",
            "\"scope\": null, \"meta\": false}, \"osc\": \"intro\"",
        ),
        (
            "\"then\": null, \"scope\": null}, \"osc\": \"intro\"",
            "\"then\": null}, \"osc\": \"intro\"",
        ),
        ("\"scope\": \"cues\"", "\"scope\": 7"),
        (
            "\"alt\": false}, \"scope\": \"cues\"",
            "\"alt\": false, \"then\": null}, \"scope\": \"cues\"",
        ),
        ("\"operation\": \"go\"", "\"operation\": \"jump\""),
        (
            "\"operation\": \"back\"",
            "\"operation\": \"back\", \"cue\": null",
        ),
        ("{\"type\": \"script\"", "{\"type\": \"macro\""),
        ("\"name\": \"Open\", ", ""),
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

    // Bounds, duplicates, and waits are project validation.
    for (from, to) in [
        ("{\"id\": 2, \"name\"", "{\"id\": 4, \"name\""),
        ("{\"id\": 2, \"name\"", "{\"id\": 0, \"name\""),
        ("\"name\": \"Open\"", "\"name\": \"  \""),
        // Ctrl+K alone clashes with the Ctrl+K chord in every panel.
        (
            "\"key\": \"F9\", \"command\": false, \"shift\": true",
            "\"key\": \"K\", \"command\": true, \"shift\": false",
        ),
        ("\"osc\": \"scores\"", "\"osc\": \"intro\""),
        ("\"osc\": \"scores\"", "\"osc\": \"Scores\""),
        ("\"key\": \"K\"", "\"key\": \"Page Up\""),
        ("\"key\": \"1\"", "\"key\": \"\""),
        ("\"scope\": \"cues\"", "\"scope\": \"timeline\""),
        (
            "{\"type\": \"audio_mute\", \"input\": 2, \"muted\": true}",
            "{\"type\": \"delay\", \"millis\": 200}",
        ),
        (
            "\"actions\": [{\"type\": \"overlay_on\", \"channel\": 1, \"input\": 4}, {\"type\": \
             \"audio_mute\", \"input\": 2, \"muted\": true}]",
            "\"actions\": []",
        ),
        ("\"script\": 2}", "\"script\": 0}"),
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
fn the_shortcut_count_is_bounded() {
    let mut project = project();
    let bare = |id: u64| Shortcut {
        id,
        name: format!("S{id}"),
        enabled: true,
        key: None,
        osc: None,
        action: ShortcutAction::Script { script: 1 },
    };
    let limit = u64::try_from(MAX_SHORTCUTS).unwrap();
    project.set_shortcuts((1..=limit + 1).map(bare).collect());
    assert!(stored(project.clone()).is_err());
    project.set_shortcuts((1..=limit).map(bare).collect());
    assert!(stored(project).is_ok());
}

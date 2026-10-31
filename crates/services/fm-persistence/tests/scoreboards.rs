//! GX-003: title scoreboard documents round-trip exactly and strictly in
//! `project.json` (schema 106); whether one names a title input is project
//! validation.

use std::{
    fs,
    num::NonZeroU128,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use fm_model::{Input, InputKind, Project, ProjectSettings, TitleInput};
use fm_persistence::{ProjectPosition, ProjectStore, RuntimeRouting, StoreError, StoredProject};
use fm_titles::{
    ClockDirection, ClockFormat, GameClockConfig, Player, Scoreboard, ScoreboardPreset, TeamState,
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
            "fm-persistence-scoreboards-{}-{sequence}-{name}",
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
            template_uri: "asset://titles/scoreboard.fmtitle".into(),
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
        "Scoreboards",
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

fn team(name: &str, abbreviation: &str) -> TeamState {
    TeamState {
        name: name.into(),
        abbreviation: abbreviation.into(),
        score: 0,
        player: Some(Player {
            number: Some(9),
            name: "Alex Doe".into(),
        }),
        roster: vec![Player {
            number: Some(9),
            name: "Alex Doe".into(),
        }],
    }
}

fn document() -> String {
    let rules = ScoreboardPreset::Basketball.rules();
    let scoreboard = Scoreboard::new(
        team("Home", "HOM"),
        team("Away", "AWY"),
        rules,
        GameClockConfig {
            direction: ClockDirection::CountDown,
            format: ClockFormat::MinutesSeconds,
            frame_rate: FrameRate::new(30, 1).unwrap(),
            initial_ms: 12 * 60 * 1_000,
            limit_ms: None,
        },
    )
    .unwrap();
    scoreboard.snapshot().to_document()
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
fn scoreboard_documents_round_trip_exactly_and_strictly() {
    let directory = TestDirectory::new("round-trip");
    let store = ProjectStore::new(directory.path("show.freemix")).unwrap();
    let mut expected = project();
    let canonical = document();
    expected
        .set_scoreboard_document(input(3), canonical.clone())
        .unwrap();
    store.save(&stored(expected.clone()).unwrap()).unwrap();
    assert_eq!(store.load().unwrap().project(), &expected);

    let text = fs::read_to_string(directory.path("show.freemix/project.json")).unwrap();
    assert!(
        text.contains("\"scoreboards\": [\n      {\"input\": 3, \"document\": \"freemix-scoreboard 1\\nrules 199 4 24000\\nperiod 1\\nrevision 0\\nlast_frame 0\\n"),
        "{text}"
    );

    let escaped = canonical.replace('\n', "\\n");
    for (from, to) in [
        ("\"scoreboards\": [", "\"scoreboard\": ["),
        ("\"document\": \"", "\"document\": \" \""),
        ("\\nrules 199 4 24000", "\\nrules 199 4 24001"),
        ("\\nperiod 1", "\\nperiod 5"),
        ("\\nrevision 0", "\\nrevision -1"),
        (
            "\\nperiod 1\\nrevision 0",
            "\\nperiod 1\\nrevision 0\\nunknown 1",
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

    // A non-canonical byte form of the same state is refused.
    let non_canonical = canonical.replace("period 1", "period 01");
    let malformed = text.replacen(&escaped, &non_canonical.replace('\n', "\\n"), 1);
    assert_ne!(malformed, text);
    fs::write(directory.path("show.freemix/project.json"), malformed).unwrap();
    assert!(matches!(
        store.load(),
        Err(StoreError::MalformedManifest { .. })
    ));
}

#[test]
fn a_scoreboard_must_name_a_title_input() {
    let mut project = project();
    project.restore_scoreboards(std::collections::BTreeMap::from([(input(9), document())]));
    assert!(stored(project).is_err());

    // An existing title accepts a document; a non-title input and an unknown
    // input are refused without changing anything.
    let mut project = self::project();
    project
        .set_scoreboard_document(input(3), document())
        .unwrap();
    assert_eq!(project.scoreboard_documents().len(), 1);
    assert!(project.remove_scoreboard_document(input(3)).is_some());
    assert!(project.scoreboard_documents().is_empty());

    // Removing a title input drops its document.
    let mut project = self::project();
    project
        .set_scoreboard_document(input(3), document())
        .unwrap();
    project.remove_input(input(3)).unwrap();
    assert!(project.scoreboard_documents().is_empty());
    stored(project).unwrap();
}

#[test]
fn a_scoreboard_document_is_validated_on_load() {
    let directory = TestDirectory::new("invalid");
    let store = ProjectStore::new(directory.path("show.freemix")).unwrap();
    let mut project = project();
    project
        .set_scoreboard_document(input(3), document())
        .unwrap();
    store.save(&stored(project).unwrap()).unwrap();

    let text = fs::read_to_string(directory.path("show.freemix/project.json")).unwrap();
    let malformed = text.replace("freemix-scoreboard 1", "freemix-scoreboard 2");
    assert_ne!(malformed, text);
    fs::write(directory.path("show.freemix/project.json"), malformed).unwrap();
    assert!(matches!(
        store.load(),
        Err(StoreError::MalformedManifest { .. })
    ));
}

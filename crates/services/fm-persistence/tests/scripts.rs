//! RC-011 at schema 54: the project's scripts round-trip exactly and
//! strictly in `project.json`; bounds and duplicate ids are project
//! validation, and a script with a syntax error is stored as written. At
//! schema 56 the show's script run slot (a waiting run or a fault) follows;
//! since schema 57 a fault keeps what the run was refused with.

use std::{
    fs,
    num::NonZeroU128,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use fm_model::{MAX_SCRIPT_SOURCE_BYTES, MAX_SCRIPTS, Project, ProjectSettings, Script};
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
            "fm-persistence-scripts-{}-{sequence}-{name}",
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
        "Scripts",
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

fn scripts() -> Vec<Script> {
    vec![
        Script {
            id: 3,
            name: "Open \"show\"".into(),
            source:
                "# open\npreview 2\r\n\ttransition fade, 25\ntitle_text \"Lower\", 1, \"a\\\\b\""
                    .into(),
        },
        Script {
            id: 1,
            name: "Draft".into(),
            // A syntax error is stored as written.
            source: "if program ==\n".into(),
        },
    ]
}

#[test]
fn scripts_round_trip_exactly_and_strictly() {
    let directory = TestDirectory::new("round-trip");
    let store = ProjectStore::new(directory.path("show.freemix")).unwrap();
    let mut expected = project();
    expected.set_scripts(scripts());
    store.save(&stored(expected.clone()).unwrap()).unwrap();
    assert_eq!(store.load().unwrap().project(), &expected);

    let text = fs::read_to_string(directory.path("show.freemix/project.json")).unwrap();
    assert!(text.contains("\"schema_version\": 106"));
    assert!(
        text.contains(
            ",\n    \"scripts\": [\n      {\"id\": 3, \"name\": \"Open \\\"show\\\"\", \"source\": \
             \"# open\\npreview 2\\r\\n\\ttransition fade, 25\\ntitle_text \\\"Lower\\\", 1, \
             \\\"a\\\\\\\\b\\\"\"},\n      {\"id\": 1, \"name\": \"Draft\", \"source\": \"if \
             program ==\\n\"}\n    ],\n    \"script_runs\": {\"last_run\": 0, \"runs\": [], \"queued\": [], \"faults\": []},\n    \"shortcuts\": [],\n    \"media_lists\": [],\n    \"title_timelines\": [],\n    \"screen_capture_restore_tokens\": [],\n    \"replay\": {\"sources\": [], \"retention_seconds\": 600, \"storage_root\": null, \"auto_return\": null, \"transition\": \"cut\"}\n  },"
        ),
        "{text}"
    );

    // An empty script list is written as an empty array.
    store.save(&stored(project()).unwrap()).unwrap();
    let empty = fs::read_to_string(directory.path("show.freemix/project.json")).unwrap();
    assert!(empty.contains(",\n    \"scripts\": [],\n    \"script_runs\": {\"last_run\": 0, \"runs\": [], \"queued\": [], \"faults\": []},\n    \"shortcuts\": [],\n    \"media_lists\": [],\n    \"title_timelines\": [],\n    \"screen_capture_restore_tokens\": [],\n    \"replay\": {\"sources\": [], \"retention_seconds\": 600, \"storage_root\": null, \"auto_return\": null, \"transition\": \"cut\"}\n  },"), "{empty}");

    store.save(&stored(expected).unwrap()).unwrap();
    for (from, to) in [
        ("\"scripts\": [", "\"script\": ["),
        ("{\"id\": 1, \"name\"", "{\"id\": \"1\", \"name\""),
        ("\"name\": \"Draft\"", "\"name\": 7"),
        ("\"source\": \"if", "\"code\": \"if"),
        ("\"name\": \"Draft\", ", ""),
        (
            "\"name\": \"Draft\",",
            "\"name\": \"Draft\", \"enabled\": true,",
        ),
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
    let without = text.replacen(",\n    \"scripts\": [", ",\n    \"ignored\": [", 1);
    fs::write(directory.path("show.freemix/project.json"), without).unwrap();
    assert!(matches!(
        store.load(),
        Err(StoreError::MalformedManifest { .. })
    ));

    // Bounds and duplicate ids are project validation.
    for (from, to) in [
        ("{\"id\": 1, \"name\"", "{\"id\": 3, \"name\""),
        ("{\"id\": 1, \"name\"", "{\"id\": 0, \"name\""),
        ("\"name\": \"Draft\"", "\"name\": \"  \""),
        ("\"name\": \"Draft\"", "\"name\": \"Dr\\u0007aft\""),
        ("\"source\": \"if", "\"source\": \"\\u0000if"),
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
fn script_bounds_are_validated() {
    let mut project = project();
    project.set_scripts(
        (1..=u64::try_from(MAX_SCRIPTS).unwrap() + 1)
            .map(|id| Script {
                id,
                name: format!("S{id}"),
                source: String::new(),
            })
            .collect(),
    );
    assert!(stored(project.clone()).is_err());
    project.set_scripts(vec![Script {
        id: 1,
        name: "Long".into(),
        source: "#".repeat(MAX_SCRIPT_SOURCE_BYTES + 1),
    }]);
    assert!(stored(project.clone()).is_err());
    project.set_scripts(vec![Script {
        id: 1,
        name: "Long".into(),
        source: "#".repeat(MAX_SCRIPT_SOURCE_BYTES),
    }]);
    assert!(stored(project).is_ok());
}

/// Schema 56: a script run waiting at a `wait` (its interpreter state) and
/// a run's fault round-trip exactly and strictly; a run that does not fit
/// its script is project validation. Schema 60: several runs, each with
/// its id, and the last id issued.
#[test]
#[allow(clippy::too_many_lines)] // one round-trip walk
fn a_waiting_run_and_a_fault_round_trip_exactly_and_strictly() {
    use fm_model::{ScriptFault, ScriptRun, ScriptRunState};
    use fm_script::{FailureKind, Limits, ResumePoint, ScriptValue, Span, Suspended};

    struct Quiet;
    impl fm_script::ScriptHost for Quiet {
        fn program(&self) -> u32 {
            1
        }
        fn preview(&self) -> u32 {
            2
        }
        fn act(&mut self, _: &fm_script::ScriptAction) -> Result<(), String> {
            Ok(())
        }
    }
    let source = "let n = 2\nlet label = \"Li\\\"ve\"\nlet on = true\nrepeat 3\n  wait 25\nend\n";
    let program = fm_script::parse(source).unwrap();
    let state = fm_script::run(&program, &mut Quiet, Limits::default())
        .suspension
        .unwrap()
        .state;
    assert_eq!(
        state,
        Suspended {
            path: vec![
                ResumePoint {
                    statement: 3,
                    branch: 2
                },
                ResumePoint {
                    statement: 0,
                    branch: 0
                }
            ],
            variables: vec![
                ("n".into(), ScriptValue::Int(2)),
                ("label".into(), ScriptValue::Str("Li\"ve".into())),
                ("on".into(), ScriptValue::Bool(true)),
            ],
            instructions: 11,
            actions: 0,
            waits: 1,
            wait_frames: 25,
        }
    );
    let directory = TestDirectory::new("script-run");
    let store = ProjectStore::new(directory.path("show.freemix")).unwrap();
    let mut expected = project();
    expected.set_scripts(vec![
        Script {
            id: 2,
            name: "Count".into(),
            source: source.into(),
        },
        Script {
            id: 3,
            name: "Pause".into(),
            source: "wait 5\n".into(),
        },
    ]);
    expected.set_script_run(ScriptRunState {
        last_run: 7,
        runs: vec![
            ScriptRun {
                run: 4,
                script: 2,
                started_frame: 40,
                due_frame: 65,
                state,
            },
            ScriptRun {
                run: 7,
                script: 3,
                started_frame: 60,
                due_frame: 65,
                state: Suspended {
                    path: vec![ResumePoint {
                        statement: 0,
                        branch: 0,
                    }],
                    variables: Vec::new(),
                    instructions: 1,
                    actions: 0,
                    waits: 1,
                    wait_frames: 5,
                },
            },
        ],
        queued: Vec::new(),
        faults: Vec::new(),
    });
    store.save(&stored(expected.clone()).unwrap()).unwrap();
    assert_eq!(store.load().unwrap().project(), &expected);
    let text = fs::read_to_string(directory.path("show.freemix/project.json")).unwrap();
    assert!(
        text.contains(
            ",\n    \"script_runs\": {\"last_run\": 7, \"runs\": [{\"run\": 4, \"script\": 2, \
             \"started_frame\": 40, \"due_frame\": 65, \"path\": [{\"statement\": 3, \"branch\": 2}, \
             {\"statement\": 0, \"branch\": 0}], \"variables\": [{\"name\": \"n\", \"int\": 2}, \
             {\"name\": \"label\", \"string\": \"Li\\\"ve\"}, {\"name\": \"on\", \"bool\": true}], \
             \"instructions\": 11, \"actions\": 0, \"waits\": 1, \"wait_frames\": 25}, {\"run\": 7, \
             \"script\": 3, \"started_frame\": 60, \"due_frame\": 65, \"path\": [{\"statement\": 0, \
             \"branch\": 0}], \"variables\": [], \"instructions\": 1, \"actions\": 0, \"waits\": 1, \
             \"wait_frames\": 5}], \"queued\": [], \"faults\": []},\n    \"shortcuts\": [],\n    \"media_lists\": [],\n    \"title_timelines\": [],\n    \"screen_capture_restore_tokens\": [],\n    \"replay\": {\"sources\": [], \"retention_seconds\": 600, \"storage_root\": null, \"auto_return\": null, \"transition\": \"cut\"}\n  },"
        ),
        "{text}"
    );

    for (from, to) in [
        ("\"script_runs\": {", "\"script_run\": {"),
        ("\"last_run\": 7, ", ""),
        ("{\"run\": 4, ", "{"),
        ("\"run\": 4, ", "\"run\": -4, "),
        ("\"int\": 2}", "\"int\": 2, \"bool\": true}"),
        ("{\"name\": \"n\", \"int\": 2}", "{\"name\": \"n\"}"),
        ("\"wait_frames\": 25}", "\"wait_frames\": 25, \"extra\": 1}"),
        ("\"branch\": 2}", "\"branch\": -2}"),
        ("\"faults\": []", "\"faults\": [{\"script\": 2}]"),
        ("\"faults\": []", "\"faults\": null"),
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
    // A run that does not fit its script, or resumes before its waits
    // end, is refused by project validation.
    for (from, to) in [
        ("\"script\": 2, \"started", "\"script\": 9, \"started"),
        // Two runs of one script, a run id past the last issued, or runs
        // out of order.
        ("\"script\": 3, \"started", "\"script\": 2, \"started"),
        ("\"last_run\": 7", "\"last_run\": 6"),
        ("{\"run\": 7, ", "{\"run\": 3, "),
        (
            "{\"statement\": 3, \"branch\": 2}",
            "{\"statement\": 2, \"branch\": 2}",
        ),
        ("\"due_frame\": 65", "\"due_frame\": 64"),
        ("\"waits\": 1", "\"waits\": 33"),
        (
            "{\"name\": \"n\", \"int\": 2}",
            "{\"name\": \"if\", \"int\": 2}",
        ),
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

    let mut faulted = expected;
    faulted.set_script_run(ScriptRunState {
        last_run: 7,
        runs: Vec::new(),
        queued: Vec::new(),
        faults: vec![ScriptFault {
            run: 5,
            script: 2,
            frame: 90,
            failure: FailureKind::Budget(fm_script::Budget::WaitFrames),
            at: Span::new(5, 3),
            detail: fm_command::FaultDetail {
                reason: None,
                message: "wait frame budget exceeded".to_owned(),
            },
        }],
    });
    store.save(&stored(faulted.clone()).unwrap()).unwrap();
    assert_eq!(store.load().unwrap().project(), &faulted);
    let text = fs::read_to_string(directory.path("show.freemix/project.json")).unwrap();
    assert!(
        text.contains(
            "\"script_runs\": {\"last_run\": 7, \"runs\": [], \"queued\": [], \"faults\": [{\"run\": 5, \
             \"script\": 2, \"frame\": 90, \"failure\": \"wait_frames\", \"line\": 5, \"column\": 3, \
             \"reason\": null, \"message\": \"wait frame budget exceeded\"}]}"
        ),
        "{text}"
    );
    let unknown = text.replacen("\"wait_frames\", \"line\"", "\"sleep\", \"line\"", 1);
    fs::write(directory.path("show.freemix/project.json"), unknown).unwrap();
    assert!(matches!(
        store.load(),
        Err(StoreError::MalformedManifest { .. })
    ));

    // Schema 57: a refused action's typed reason and its message round-trip
    // exactly; a reason no token names, a missing field, or a message out
    // of bounds is refused.
    faulted.set_script_run(ScriptRunState {
        last_run: 7,
        runs: Vec::new(),
        queued: Vec::new(),
        faults: vec![ScriptFault {
            run: 5,
            script: 2,
            frame: 90,
            failure: FailureKind::Refused,
            at: Span::new(2, 1),
            detail: fm_command::FaultDetail {
                reason: Some(fm_command::RefusalReason::InputInUse {
                    input: 4,
                    usage: fm_command::InputUse::Overlay { channel: 2 },
                }),
                message: "input 4 is the \"source\" of overlay 2".to_owned(),
            },
        }],
    });
    store.save(&stored(faulted.clone()).unwrap()).unwrap();
    assert_eq!(store.load().unwrap().project(), &faulted);
    let text = fs::read_to_string(directory.path("show.freemix/project.json")).unwrap();
    assert!(
        text.contains(
            "\"failure\": \"refused\", \"line\": 2, \"column\": 1, \
             \"reason\": \"input_in_use:4:overlay:2\", \
             \"message\": \"input 4 is the \\\"source\\\" of overlay 2\"}]}"
        ),
        "{text}"
    );
    for (from, to) in [
        (
            "\"input_in_use:4:overlay:2\"",
            "\"input_in_use:4:overlay:0\"",
        ),
        ("\"input_in_use:4:overlay:2\"", "\"bogus\""),
        ("\"input_in_use:4:overlay:2\"", "7"),
        ("\"reason\": \"input_in_use:4:overlay:2\", ", ""),
        (", \"message\": \"input 4", ", \"text\": \"input 4"),
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
    let long = "x".repeat(fm_command::MAX_FAULT_MESSAGE_BYTES + 1);
    for message in ["", " padded", "two\\nlines", long.as_str()] {
        let invalid = text.replacen(
            "\"input 4 is the \\\"source\\\" of overlay 2\"",
            &format!("\"{message}\""),
            1,
        );
        assert_ne!(invalid, text, "{message}");
        fs::write(directory.path("show.freemix/project.json"), invalid).unwrap();
        assert!(
            matches!(store.load(), Err(StoreError::Validation(_))),
            "{message}: {:?}",
            store.load()
        );
    }
}

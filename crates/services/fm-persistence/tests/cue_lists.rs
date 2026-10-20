//! RC-017 at schema 48 (media actions and millisecond delays; schema 55
//! script steps): the project's operator cue lists, with their standby
//! pointer and run state, round-trip exactly and strictly in
//! `project.json`; references are project validation.

use std::{
    fs,
    num::NonZeroU128,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use fm_model::{
    AudioBus, Cue, CueAction, CueArmed, CueList, CueMediaOperation, CuePending, CueRunState, Input,
    InputKind, Output, Project, ProjectSettings, RecorderSlot, Rgba8, Scene, SimulatedAudio,
    SimulatedInput, SimulatedVideo, SolidColor, StartupPolicy, StingerSlotNumber, TransitionEffect,
};
use fm_persistence::{ProjectPosition, ProjectStore, RuntimeRouting, StoreError, StoredProject};
use fm_types::{
    AudioFormat, BusId, ChannelLayout, ColorMetadata, FrameRate, InputId, OutputId, PixelFormat,
    ProjectId, SampleFormat, SampleRate, ScanMode, SceneId, VideoDimensions, VideoFormat,
};

static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(name: &str) -> Self {
        let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "fm-persistence-cues-{}-{sequence}-{name}",
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

fn id(value: u128) -> NonZeroU128 {
    NonZeroU128::new(value).unwrap()
}

fn input(value: u128) -> InputId {
    InputId::new(id(value))
}

fn project() -> Project {
    let frame_rate = FrameRate::new(30, 1).unwrap();
    let mut project = Project::new(
        ProjectId::new(id(1)),
        "Cues",
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
        id: input(12),
        name: "Clip".into(),
        kind: InputKind::Media {
            asset_uri: "asset://clip.mov".into(),
            playback: fm_model::MediaPlaybackSettings::default(),
        },
        required_capabilities: Vec::new(),
    });
    for (value, name) in [(10, "Host"), (11, "Guest")] {
        project.add_input(Input {
            id: input(value),
            name: name.into(),
            kind: InputKind::Simulated(SimulatedInput::new(
                SimulatedVideo::Solid(SolidColor::new(1, 2, 3, 255)),
                SimulatedAudio::Sine { frequency_hz: 440 },
            )),
            required_capabilities: Vec::new(),
        });
    }
    project.add_scene(Scene {
        id: SceneId::new(id(20)),
        name: "Scene".into(),
        background: Rgba8::OPAQUE_BLACK,
        layers: Vec::new(),
        animation: fm_model::SceneAnimation::default(),
    });
    project.add_audio_bus(AudioBus {
        id: BusId::new(id(30)),
        name: "Master".into(),
        sends: Vec::new(),
    });
    project.add_output(Output {
        id: OutputId::new(id(40)),
        name: "Program".into(),
        video_source: SceneId::new(id(20)),
        audio_source: BusId::new(id(30)),
        startup: StartupPolicy::Stopped,
        required_capabilities: Vec::new(),
    });
    project.set_scripts(vec![fm_model::Script {
        id: 5,
        name: "Open".into(),
        source: "cut 2\n".into(),
    }]);
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

fn lists() -> Vec<CueList> {
    vec![
        CueList {
            id: 1,
            name: "Show \"A\"".into(),
            cues: vec![
                Cue {
                    id: 1,
                    name: "Open".into(),
                    actions: vec![
                        CueAction::Preview { input: input(10) },
                        CueAction::Take {
                            input: Some(input(11)),
                            effect: TransitionEffect::Fade,
                            duration_frames: 25,
                        },
                        CueAction::OverlayOn {
                            channel: 2,
                            input: input(10),
                        },
                        CueAction::AudioGain {
                            input: input(10),
                            gain_millidb: -6_000,
                        },
                        CueAction::AudioMute {
                            input: input(11),
                            muted: true,
                        },
                        CueAction::Record {
                            recorder: RecorderSlot::new(2).unwrap(),
                            start: true,
                        },
                        CueAction::Wait { frames: 30 },
                        CueAction::Take {
                            input: None,
                            effect: TransitionEffect::Stinger(StingerSlotNumber::new(3).unwrap()),
                            duration_frames: 40,
                        },
                        CueAction::OverlayOff { channel: 2 },
                    ],
                    auto_follow: true,
                },
                Cue {
                    id: 4,
                    name: "Close".into(),
                    // Cue 1 follows on into this cue on its last frame,
                    // which already takes Program: this cue takes it only
                    // after its own first wait.
                    actions: vec![
                        CueAction::FadeToBlack {
                            active: true,
                            duration_frames: 50,
                        },
                        CueAction::Wait { frames: 2 },
                        CueAction::PlaylistStart,
                        CueAction::Wait { frames: 2 },
                        CueAction::PlaylistStop,
                        CueAction::Take {
                            input: Some(input(10)),
                            effect: TransitionEffect::Cut,
                            duration_frames: 0,
                        },
                    ],
                    auto_follow: false,
                },
            ],
            state: CueRunState {
                standby: Some(4),
                live: Some(1),
                pending: Some(CuePending {
                    cue: 1,
                    action: 7,
                    due_frame: 120,
                }),
                played: vec![1],
                fault: None,
                // Schema 58: cue 4 is armed to fire on frame 300.
                armed: Some(CueArmed {
                    cue: 4,
                    due_frame: 300,
                }),
            },
        },
        CueList {
            id: 9,
            name: "Empty".into(),
            cues: Vec::new(),
            state: CueRunState::default(),
        },
        clips_list(),
    ]
}

/// A list of media transport actions and a millisecond delay, mid-wait.
fn clips_list() -> CueList {
    CueList {
        id: 12,
        name: "Clips".into(),
        cues: vec![Cue {
            id: 1,
            name: "Roll".into(),
            actions: vec![
                CueAction::Media {
                    input: input(12),
                    operation: CueMediaOperation::Restart,
                },
                CueAction::Delay { millis: 1_500 },
                CueAction::Media {
                    input: input(12),
                    operation: CueMediaOperation::Seek { frame: 250 },
                },
                CueAction::Media {
                    input: input(12),
                    operation: CueMediaOperation::Pause,
                },
                CueAction::Media {
                    input: input(12),
                    operation: CueMediaOperation::Play,
                },
                CueAction::RunScript { script: 5 },
            ],
            auto_follow: false,
        }],
        state: CueRunState {
            standby: None,
            live: Some(1),
            pending: Some(CuePending {
                cue: 1,
                action: 2,
                due_frame: 45,
            }),
            played: vec![1],
            fault: None,
            armed: None,
        },
    }
}

#[test]
#[allow(clippy::too_many_lines)] // one exhaustive strict-decoder scenario
fn cue_lists_round_trip_exactly_and_strictly() {
    let directory = TestDirectory::new("round-trip");
    let store = ProjectStore::new(directory.path("show.freemix")).unwrap();
    let mut expected = project();
    expected.set_cue_lists(lists());
    store.save(&stored(expected.clone())).unwrap();
    assert_eq!(store.load().unwrap().project(), &expected);

    let text = fs::read_to_string(directory.path("show.freemix/project.json")).unwrap();
    assert!(text.contains("\"schema_version\": 69"));
    assert!(text.contains("\"cue_lists\": ["), "{text}");
    assert!(
        text.contains("{\"type\": \"wait\", \"frames\": 30}"),
        "{text}"
    );
    assert!(
        text.contains("\"effect\": \"stinger\", \"slot\": 3"),
        "{text}"
    );
    assert!(
        text.contains("\"pending\": {\"cue\": 1, \"action\": 7, \"due_frame\": 120}"),
        "{text}"
    );
    for token in [
        "{\"type\": \"media_restart\", \"input\": 12}",
        "{\"type\": \"delay\", \"millis\": 1500}",
        "{\"type\": \"media_seek\", \"input\": 12, \"frame\": 250}",
        "{\"type\": \"media_pause\", \"input\": 12}",
        "{\"type\": \"media_play\", \"input\": 12}",
        "{\"type\": \"run_script\", \"script\": 5}",
        "\"pending\": {\"cue\": 1, \"action\": 2, \"due_frame\": 45}",
        "\"armed\": {\"cue\": 4, \"due_frame\": 300}}",
        "\"fault\": null, \"armed\": null}",
    ] {
        assert!(text.contains(token), "{token}: {text}");
    }

    for (from, to) in [
        (
            "{\"type\": \"wait\", \"frames\": 30}",
            "{\"type\": \"sleep\", \"frames\": 30}",
        ),
        (
            "{\"type\": \"wait\", \"frames\": 30}",
            "{\"type\": \"wait\"}",
        ),
        (
            "{\"type\": \"wait\", \"frames\": 30}",
            "{\"type\": \"wait\", \"frames\": 30, \"loop\": true}",
        ),
        (
            "{\"type\": \"wait\", \"frames\": 30}",
            "{\"type\": \"wait\", \"frames\": -1}",
        ),
        ("\"effect\": \"fade\"", "\"effect\": \"dissolve\""),
        ("\"auto_follow\": true", "\"auto_follow\": 1"),
        ("\"played\": [1]", "\"played\": \"1\""),
        (
            "{\"type\": \"media_seek\", \"input\": 12, \"frame\": 250}",
            "{\"type\": \"media_seek\", \"input\": 12}",
        ),
        (
            "{\"type\": \"media_play\", \"input\": 12}",
            "{\"type\": \"media_stop\", \"input\": 12}",
        ),
        (
            "{\"type\": \"delay\", \"millis\": 1500}",
            "{\"type\": \"delay\", \"millis\": 1.5}",
        ),
        (
            "{\"type\": \"run_script\", \"script\": 5}",
            "{\"type\": \"run_script\", \"script\": \"Open\"}",
        ),
        (
            "{\"type\": \"run_script\", \"script\": 5}",
            "{\"type\": \"run_script\"}",
        ),
        (",\n    \"cue_lists\": [", ",\n    \"cue_list\": ["),
        (
            "\"armed\": {\"cue\": 4, \"due_frame\": 300}",
            "\"armed\": {\"cue\": 4}",
        ),
        (
            "\"armed\": {\"cue\": 4, \"due_frame\": 300}",
            "\"armed\": 4",
        ),
        ("\"fault\": null, \"armed\": null}", "\"fault\": null}"),
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

    // Bounds, references, and run state consistency are project validation.
    for (from, to) in [
        (
            "{\"type\": \"wait\", \"frames\": 30}",
            "{\"type\": \"wait\", \"frames\": 0}",
        ),
        ("\"input\": 11, \"effect\"", "\"input\": 99, \"effect\""),
        (
            "{\"type\": \"delay\", \"millis\": 1500}",
            "{\"type\": \"delay\", \"millis\": 0}",
        ),
        (
            "{\"type\": \"delay\", \"millis\": 1500}",
            "{\"type\": \"delay\", \"millis\": 3600001}",
        ),
        (
            "{\"type\": \"media_pause\", \"input\": 12}",
            "{\"type\": \"media_pause\", \"input\": 10}",
        ),
        (
            "{\"type\": \"run_script\", \"script\": 5}",
            "{\"type\": \"run_script\", \"script\": 0}",
        ),
        (
            "{\"type\": \"run_script\", \"script\": 5}",
            "{\"type\": \"run_script\", \"script\": 6}",
        ),
        ("\"standby\": 4", "\"standby\": 5"),
        ("\"armed\": {\"cue\": 4,", "\"armed\": {\"cue\": 5,"),
        ("\"action\": 7", "\"action\": 6"),
        ("\"played\": [1]", "\"played\": [4, 1]"),
        ("\"channel\": 2, \"input\"", "\"channel\": 9, \"input\""),
        (
            "{\"id\": 4, \"name\": \"Close\"",
            "{\"id\": 1, \"name\": \"Close\"",
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
}

#[test]
fn a_project_whose_cue_names_a_missing_input_is_refused() {
    let mut project = project();
    let mut lists = lists();
    lists[0].cues[0].actions[0] = CueAction::Preview { input: input(77) };
    project.set_cue_lists(lists);
    assert!(
        StoredProject::from_project(
            project,
            RuntimeRouting::default(),
            ProjectPosition::default(),
            Vec::new(),
        )
        .is_err()
    );
}

/// Schema 57: a cue fault keeps what its deferred action was refused with,
/// its typed reason (a stable token or `null`) and its message, exactly
/// and strictly.
#[test]
fn a_cue_fault_round_trips_its_refusal_reason_and_message() {
    let directory = TestDirectory::new("fault");
    let store = ProjectStore::new(directory.path("show.freemix")).unwrap();
    let mut expected = project();
    let mut faulted = lists();
    faulted[0].state.pending = None;
    faulted[0].state.fault = Some(fm_model::CueFault {
        cue: 1,
        action: 7,
        detail: fm_command::FaultDetail {
            reason: Some(fm_command::RefusalReason::StingerRealizing { slot: 3 }),
            message: "Stinger slot 3 has not finished realizing".to_owned(),
        },
    });
    expected.set_cue_lists(faulted.clone());
    store.save(&stored(expected.clone())).unwrap();
    assert_eq!(store.load().unwrap().project(), &expected);
    let text = fs::read_to_string(directory.path("show.freemix/project.json")).unwrap();
    let fault = "\"fault\": {\"cue\": 1, \"action\": 7, \"reason\": \"stinger_realizing:3\", \
                 \"message\": \"Stinger slot 3 has not finished realizing\"}";
    assert!(text.contains(fault), "{text}");

    for (from, to) in [
        ("\"stinger_realizing:3\"", "\"stinger_realizing:0\""),
        ("\"stinger_realizing:3\"", "\"stinger_ready:3\""),
        ("\"reason\": \"stinger_realizing:3\", ", ""),
        (", \"message\": \"Stinger", ", \"detail\": \"Stinger"),
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
    for message in ["", "a\\tb", long.as_str()] {
        let invalid = text.replacen(
            "\"Stinger slot 3 has not finished realizing\"",
            &format!("\"{message}\""),
            1,
        );
        fs::write(directory.path("show.freemix/project.json"), invalid).unwrap();
        assert!(
            matches!(store.load(), Err(StoreError::Validation(_))),
            "{message}: {:?}",
            store.load()
        );
    }

    // A fault without a typed reason keeps its message alone.
    if let Some(fault) = &mut faulted[0].state.fault {
        fault.detail.reason = None;
    }
    expected.set_cue_lists(faulted);
    store.save(&stored(expected.clone())).unwrap();
    assert_eq!(store.load().unwrap().project(), &expected);
    let text = fs::read_to_string(directory.path("show.freemix/project.json")).unwrap();
    assert!(
        text.contains("\"action\": 7, \"reason\": null, \"message\": \"Stinger"),
        "{text}"
    );
}

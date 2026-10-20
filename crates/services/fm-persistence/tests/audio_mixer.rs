//! AU-001 and SW-004 at schemas 39, 42, and 50: strip pan, labels, groups
//! (with solo and nesting), the solo mode, the monitor output, and the
//! Fade-to-Black output selection round-trip exactly and strictly in
//! `project.json`.

use std::{
    fs,
    num::NonZeroU128,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use fm_model::{
    AudioBus, AudioGroup, AudioGroupNumber, AudioMonitorOutput, AudioSoloMode, Input,
    InputAudioStripState, InputGainMilliDb, InputKind, InputPanBasisPoints, Output, Project,
    ProjectSettings, Rgba8, Scene, SimulatedAudio, SimulatedInput, SimulatedVideo, SolidColor,
    StartupPolicy,
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
            "fm-persistence-mixer-{}-{sequence}-{name}",
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

fn project() -> Project {
    let frame_rate = FrameRate::new(30, 1).unwrap();
    let mut project = Project::new(
        ProjectId::new(id(1)),
        "Mixer",
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
    for (value, name) in [(10, "Host"), (11, "Guest")] {
        project.add_input(Input {
            id: InputId::new(id(value)),
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
    for (value, name) in [(40, "Program"), (41, "Clean")] {
        project.add_output(Output {
            id: OutputId::new(id(value)),
            name: name.into(),
            video_source: SceneId::new(id(20)),
            audio_source: BusId::new(id(30)),
            startup: StartupPolicy::Stopped,
            required_capabilities: Vec::new(),
        });
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

#[test]
#[allow(clippy::too_many_lines)]
fn pan_labels_groups_and_fade_to_black_outputs_round_trip_exactly() {
    let directory = TestDirectory::new("round-trip");
    let store = ProjectStore::new(directory.path("show.freemix")).unwrap();
    let mut expected = project();
    let host = InputId::new(id(10));
    let hosts = AudioGroupNumber::new(2).unwrap();
    assert!(expected.set_input_audio_strip(
        host,
        InputAudioStripState {
            pan: InputPanBasisPoints::new(-2_500).unwrap(),
            ..InputAudioStripState::default()
        },
    ));
    expected
        .set_input_audio_label(host, "Host \"mic\" 1".into())
        .unwrap();
    assert!(expected.set_input_audio_group(host, Some(hosts)));
    expected
        .set_audio_group(
            hosts,
            AudioGroup {
                name: "Hosts".into(),
                gain: InputGainMilliDb::new(-6_000).unwrap(),
                muted: true,
                soloed: true,
                parent: AudioGroupNumber::new(3),
            },
        )
        .unwrap();
    expected.set_audio_solo_mode(AudioSoloMode::Pfl);
    expected.set_fade_to_black_output(OutputId::new(id(41)), false);
    // Output 40 fades over its own 45 frames on the eased curve (schema 45).
    expected.set_fade_to_black_output_duration(OutputId::new(id(40)), Some(45));
    expected.set_fade_to_black_curve(fm_types::FadeToBlackCurve::Ease);
    store.save(&stored(expected.clone())).unwrap();

    let loaded = store.load().unwrap();
    assert_eq!(loaded.project(), &expected);
    assert_eq!(
        loaded.project().input_audio_label(host),
        Some("Host \"mic\" 1")
    );
    assert_eq!(loaded.project().input_audio_group(host), Some(hosts));
    assert_eq!(loaded.project().audio_solo_mode(), AudioSoloMode::Pfl);
    assert_eq!(
        loaded.project().audio_groups().get(hosts).parent,
        AudioGroupNumber::new(3)
    );
    assert!(
        !loaded
            .project()
            .fade_to_black_affects(OutputId::new(id(41)))
    );
    assert!(
        loaded
            .project()
            .fade_to_black_affects(OutputId::new(id(40)))
    );
    assert_eq!(
        loaded
            .project()
            .fade_to_black_output_durations()
            .get(&OutputId::new(id(40))),
        Some(&45)
    );
    assert_eq!(
        loaded.project().fade_to_black_curve(),
        fm_types::FadeToBlackCurve::Ease
    );

    let text = fs::read_to_string(directory.path("show.freemix/project.json")).unwrap();
    assert!(text.contains("\"schema_version\": 64"));
    assert!(text.contains(
        "{\"input\": 10, \"gain_milli_db\": 0, \"balance_basis_points\": 0, \"pan_basis_points\": -2500, \"delay_samples\": 0, \"muted\": false, \"soloed\": false, \"follow_video\": true, \"label\": \"Host \\\"mic\\\" 1\", \"group\": 2}"
    ), "{text}");
    assert!(text.contains("\"follow_video\": true, \"label\": \"\", \"group\": null}"));
    assert!(text.contains(
        "\"audio_groups\": [\n      {\"name\": \"Group 1\", \"gain_milli_db\": 0, \"muted\": false, \"soloed\": false, \"parent\": null},\n      {\"name\": \"Hosts\", \"gain_milli_db\": -6000, \"muted\": true, \"soloed\": true, \"parent\": 3},\n      {\"name\": \"Group 3\", \"gain_milli_db\": 0, \"muted\": false, \"soloed\": false, \"parent\": null},\n      {\"name\": \"Group 4\", \"gain_milli_db\": 0, \"muted\": false, \"soloed\": false, \"parent\": null}\n    ],\n    \"audio_solo_mode\": \"pfl\",\n    \"audio_monitor_output\": {\"device\": null, \"gain_milli_db\": 0, \"muted\": false},\n    \"fade_to_black_excluded_outputs\": [41],\n    \"fade_to_black_output_durations\": [{\"output\": 40, \"duration_frames\": 45}],\n    \"fade_to_black_curve\": \"ease\",\n    \"display_outputs\": [],\n    \"live_lan\": {\"enabled\": false, \"source\": \"program\", \"max_viewers\": 8, \"pin\": null},\n    \"scripts\": [],\n    \"script_runs\": {\"last_run\": 0, \"runs\": [], \"faults\": []},\n    \"shortcuts\": [],\n    \"media_lists\": []\n  },"
    ), "{text}");

    for (from, to) in [
        ("\"group\": 2}", "\"group\": 0}"),
        ("\"group\": 2}", "\"group\": 5}"),
        ("\"group\": 2}", "\"group\": \"2\"}"),
        ("\"pan_basis_points\": -2500", "\"pan_basis_points\": 10001"),
        (
            "{\"name\": \"Group 4\", \"gain_milli_db\": 0,",
            "{\"name\": \"Group 4\", \"gain_milli_db\": 24001,",
        ),
        (
            ",\n      {\"name\": \"Group 4\", \"gain_milli_db\": 0, \"muted\": false, \"soloed\": false, \"parent\": null}",
            "",
        ),
        (
            "\"soloed\": true, \"parent\": 3}",
            "\"soloed\": 1, \"parent\": 3}",
        ),
        ("\"soloed\": true, \"parent\": 3}", "\"parent\": 3}"),
        (
            "\"soloed\": true, \"parent\": 3}",
            "\"soloed\": true, \"parent\": 0}",
        ),
        (
            "\"soloed\": true, \"parent\": 3}",
            "\"soloed\": true, \"parent\": 5}",
        ),
        (
            "\"soloed\": true, \"parent\": 3}",
            "\"soloed\": true, \"parent\": \"3\"}",
        ),
        ("\"soloed\": true, \"parent\": 3}", "\"soloed\": true}"),
        (
            "\"audio_solo_mode\": \"pfl\"",
            "\"audio_solo_mode\": \"PFL\"",
        ),
        ("\"audio_solo_mode\": \"pfl\"", "\"audio_solo_mode\": 1"),
        ("\n    \"audio_solo_mode\": \"pfl\",", ""),
        (
            "\"fade_to_black_excluded_outputs\": [41]",
            "\"fade_to_black_excluded_outputs\": [41, 41]",
        ),
        (
            "\"fade_to_black_excluded_outputs\": [41]",
            "\"fade_to_black_excluded_outputs\": [0]",
        ),
        (
            "\"fade_to_black_excluded_outputs\": [41]",
            "\"fade_to_black_excluded_outputs\": {}",
        ),
        (",\n    \"fade_to_black_excluded_outputs\": [41]", ""),
        ("\"duration_frames\": 45}", "\"duration_frames\": \"45\"}"),
        (
            "{\"output\": 40, \"duration_frames\": 45}",
            "{\"output\": 40}",
        ),
        (
            "{\"output\": 40, \"duration_frames\": 45}",
            "{\"output\": 40, \"duration_frames\": 45}, {\"output\": 40, \"duration_frames\": 45}",
        ),
        (
            "{\"output\": 40, \"duration_frames\": 45}",
            "{\"output\": 40, \"duration_frames\": 45, \"curve\": \"ease\"}",
        ),
        (
            "\"fade_to_black_curve\": \"ease\"",
            "\"fade_to_black_curve\": \"Ease\"",
        ),
        (
            "\"fade_to_black_curve\": \"ease\"",
            "\"fade_to_black_curve\": null",
        ),
        (",\n    \"fade_to_black_curve\": \"ease\"", ""),
        (
            ",\n    \"fade_to_black_output_durations\": [{\"output\": 40, \"duration_frames\": 45}]",
            "",
        ),
    ] {
        let malformed = text.replacen(from, to, 1);
        assert_ne!(malformed, text, "{from}");
        fs::write(directory.path("show.freemix/project.json"), malformed).unwrap();
        assert!(
            matches!(store.load(), Err(StoreError::MalformedManifest { .. })),
            "{from} -> {to}"
        );
    }

    // Names, labels, and output references are project validation.
    for (from, to) in [
        ("\"name\": \"Hosts\"", "\"name\": \" \""),
        (
            "\"name\": \"Hosts\"",
            "\"name\": \"A group name longer than 32 bytes\"",
        ),
        (
            "\"label\": \"Host \\\"mic\\\" 1\"",
            "\"label\": \"tab\\there\"",
        ),
        (
            "\"fade_to_black_excluded_outputs\": [41]",
            "\"fade_to_black_excluded_outputs\": [99]",
        ),
        ("\"duration_frames\": 45}", "\"duration_frames\": 0}"),
        ("\"duration_frames\": 45}", "\"duration_frames\": 3601}"),
        ("{\"output\": 40,", "{\"output\": 99,"),
        // Group 3 inside group 2 inside group 3.
        (
            "{\"name\": \"Group 3\", \"gain_milli_db\": 0, \"muted\": false, \"soloed\": false, \"parent\": null}",
            "{\"name\": \"Group 3\", \"gain_milli_db\": 0, \"muted\": false, \"soloed\": false, \"parent\": 2}",
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
fn the_monitor_output_round_trips_exactly_and_strictly() {
    let directory = TestDirectory::new("monitor-output");
    let store = ProjectStore::new(directory.path("show.freemix")).unwrap();
    let mut expected = project();
    expected
        .set_audio_monitor_output(AudioMonitorOutput {
            device: Some("hw:CARD=Headset,DEV=0".into()),
            gain: InputGainMilliDb::new(-6_000).unwrap(),
            muted: true,
        })
        .unwrap();
    let expected = stored(expected);
    store.save(&expected).unwrap();
    assert_eq!(store.load().unwrap(), expected);

    let text = fs::read_to_string(directory.path("show.freemix/project.json")).unwrap();
    let encoded = "\"audio_monitor_output\": {\"device\": \"hw:CARD=Headset,DEV=0\", \"gain_milli_db\": -6000, \"muted\": true}";
    assert!(
        text.contains(&format!(
            "\"audio_solo_mode\": \"sip\",\n    {encoded},\n    \"fade_to_black_excluded_outputs\""
        )),
        "{text}"
    );

    // No device is `null`.
    let meter_only = text.replacen(
        "\"device\": \"hw:CARD=Headset,DEV=0\"",
        "\"device\": null",
        1,
    );
    fs::write(directory.path("show.freemix/project.json"), &meter_only).unwrap();
    assert_eq!(
        store
            .load()
            .unwrap()
            .project()
            .audio_monitor_output()
            .device,
        None
    );

    for (from, to) in [
        ("\"device\": \"hw:CARD=Headset,DEV=0\"", "\"device\": 1"),
        ("\"device\": \"hw:CARD=Headset,DEV=0\", ", ""),
        ("\"gain_milli_db\": -6000", "\"gain_milli_db\": 24001"),
        ("\"gain_milli_db\": -6000", "\"gain_milli_db\": \"-6000\""),
        ("\"muted\": true}", "\"muted\": 1}"),
        ("\"muted\": true}", "\"muted\": true, \"solo\": false}"),
        (encoded, "\"audio_monitor_output\": null"),
        (&format!(",\n    {encoded}"), ""),
    ] {
        let malformed = text.replacen(from, to, 1);
        assert_ne!(malformed, text, "{from}");
        fs::write(directory.path("show.freemix/project.json"), malformed).unwrap();
        assert!(
            matches!(store.load(), Err(StoreError::MalformedManifest { .. })),
            "{from} -> {to}"
        );
    }

    // An unusable device name is project validation.
    for device in ["", "two words", "caf\u{e9}"] {
        let invalid = text.replacen(
            "\"device\": \"hw:CARD=Headset,DEV=0\"",
            &format!("\"device\": \"{device}\""),
            1,
        );
        fs::write(directory.path("show.freemix/project.json"), invalid).unwrap();
        assert!(
            matches!(store.load(), Err(StoreError::Validation(_))),
            "{device:?}: {:?}",
            store.load()
        );
    }
}

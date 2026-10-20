//! IN-003 at schema 36: a media input's playback settings (trim, end
//! action, end-trigger lead, speed, audio policy and track, growing-file
//! options, start position) round-trip exactly and strictly in
//! `project.json`.

use std::{
    fs,
    num::NonZeroU128,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use fm_model::{
    Input, InputKind, MediaAudioSpeedPolicy, MediaEndAction, MediaGrowing, MediaPlaybackSettings,
    Project, ProjectSettings,
};
use fm_persistence::{ProjectPosition, ProjectStore, RuntimeRouting, StoredProject};
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
            "fm-persistence-media-{}-{sequence}-{name}",
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

fn input_id(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn project(playback: MediaPlaybackSettings, second: MediaPlaybackSettings) -> Project {
    let frame_rate = FrameRate::new(30, 1).unwrap();
    let mut project = Project::new(
        ProjectId::new(NonZeroU128::new(1).unwrap()),
        "Media",
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
    for (id, settings) in [(1, playback), (2, second)] {
        project.add_input(Input {
            id: input_id(id),
            name: format!("Clip {id}"),
            kind: InputKind::Media {
                asset_uri: format!("asset://media/clip-{id}.mov"),
                playback: settings,
            },
            required_capabilities: Vec::new(),
        });
    }
    project
}

fn configured() -> MediaPlaybackSettings {
    MediaPlaybackSettings {
        trim_in: 12,
        trim_out: Some(480),
        end_action: MediaEndAction::Loop,
        end_trigger_lead: Some(30),
        speed_basis_points: 25_000,
        audio_policy: MediaAudioSpeedPolicy::Resample,
        audio_stream: Some(2),
        growing: Some(MediaGrowing {
            live_edge_offset_frames: 4,
            idle_completion_refreshes: 90,
        }),
        start_frame: 40,
    }
}

#[test]
fn media_playback_settings_round_trip_exactly_and_are_strict() {
    let directory = TestDirectory::new("settings");
    let store = ProjectStore::new(directory.path("show.freemix")).unwrap();
    let expected = project(configured(), MediaPlaybackSettings::default());
    store
        .save(
            &StoredProject::from_project(
                expected.clone(),
                RuntimeRouting::default(),
                ProjectPosition::default(),
                Vec::new(),
            )
            .unwrap(),
        )
        .unwrap();
    let loaded = store.load().unwrap();
    assert_eq!(
        loaded.project().media_playback(input_id(1)),
        Some(&configured())
    );
    assert_eq!(
        loaded.project().media_playback(input_id(2)),
        Some(&MediaPlaybackSettings::default())
    );
    let manifest = directory.path("show.freemix/project.json");
    let text = fs::read_to_string(&manifest).unwrap();
    assert!(text.contains("\"schema_version\": 65"));
    let configured_fragment = "{\"type\": \"media\", \"asset_uri\": \"asset://media/clip-1.mov\", \
         \"playback\": {\"trim_in\": 12, \"trim_out\": 480, \"end_action\": \"loop\", \
         \"end_trigger_lead\": 30, \"speed_basis_points\": 25000, \"audio_policy\": \"resample\", \
         \"audio_stream\": 2, \"growing\": {\"live_edge_offset_frames\": 4, \
         \"idle_completion_refreshes\": 90}, \"start_frame\": 40}}";
    assert!(text.contains(configured_fragment), "{text}");
    let default_fragment = "\"playback\": {\"trim_in\": 0, \"trim_out\": null, \
         \"end_action\": \"hold\", \"end_trigger_lead\": null, \"speed_basis_points\": 10000, \
         \"audio_policy\": \"mute\", \"audio_stream\": null, \"growing\": null, \
         \"start_frame\": 0}";
    assert!(text.contains(default_fragment), "{text}");

    for (from, to) in [
        // Trim out follows trim in; the start frame lies inside the trim.
        ("\"trim_out\": 480", "\"trim_out\": 12"),
        ("\"start_frame\": 40", "\"start_frame\": 11"),
        ("\"start_frame\": 40", "\"start_frame\": 480"),
        // Speed is 0.25x through 4x.
        (
            "\"speed_basis_points\": 25000",
            "\"speed_basis_points\": 2499",
        ),
        (
            "\"speed_basis_points\": 25000",
            "\"speed_basis_points\": 40001",
        ),
        // Closed vocabularies.
        ("\"end_action\": \"loop\"", "\"end_action\": \"pause\""),
        (
            "\"audio_policy\": \"resample\"",
            "\"audio_policy\": \"Resample\"",
        ),
        // Bounded lead and growing options.
        ("\"end_trigger_lead\": 30", "\"end_trigger_lead\": 3601"),
        (
            "\"live_edge_offset_frames\": 4",
            "\"live_edge_offset_frames\": 0",
        ),
        (
            "\"idle_completion_refreshes\": 90",
            "\"idle_completion_refreshes\": 100001",
        ),
        // Every field is required, and nothing else is accepted.
        ("\"audio_stream\": 2, ", ""),
        (
            "\"start_frame\": 40}",
            "\"start_frame\": 40, \"volume\": 1}",
        ),
        (
            "\"growing\": {\"live_edge_offset_frames\": 4, ",
            "\"growing\": {",
        ),
        (
            ", \"playback\": {\"trim_in\": 12",
            ", \"settings\": {\"trim_in\": 12",
        ),
        ("\"trim_in\": 12", "\"trim_in\": -1"),
    ] {
        assert!(text.contains(from), "{from}");
        fs::write(&manifest, text.replacen(from, to, 1)).unwrap();
        assert!(store.load().is_err(), "{from} -> {to} loaded");
    }
}

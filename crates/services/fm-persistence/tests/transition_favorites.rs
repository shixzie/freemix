//! SW-002 and SW-005 at schema 35: the four favourite transition buttons
//! round-trip exactly and strictly in `project.json`, and every overlay
//! channel transition (including the motion family) is a runtime token.

use std::{
    fs,
    num::NonZeroU128,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use fm_model::{
    Project, ProjectSettings, StingerSlotNumber, TransitionEffect, TransitionFavorite,
    TransitionFavorites,
};
use fm_persistence::{
    ProjectPosition, ProjectStore, RuntimeFadeToBlack, RuntimeManualTransitions,
    RuntimeOverlayTransition, RuntimeOverlays, RuntimeRouting, StoredProject,
};
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
            "fm-persistence-favourites-{}-{sequence}-{name}",
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
        "Favourites",
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

fn favorite(effect: TransitionEffect, duration_frames: u32) -> TransitionFavorite {
    TransitionFavorite::new(effect, duration_frames).unwrap()
}

#[test]
fn favourites_round_trip_exactly_and_are_strict() {
    let directory = TestDirectory::new("favourites");
    let store = ProjectStore::new(directory.path("show.freemix")).unwrap();
    let mut expected = project();
    expected.set_transition_favorites(TransitionFavorites::new([
        favorite(TransitionEffect::CrossZoom, 12),
        favorite(TransitionEffect::Cut, 1),
        favorite(
            TransitionEffect::Stinger(StingerSlotNumber::new(3).unwrap()),
            45,
        ),
        favorite(TransitionEffect::VerticalSlide, 3_600),
    ]));
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
        loaded.project().transition_favorites(),
        expected.transition_favorites()
    );
    let manifest = directory.path("show.freemix/project.json");
    let text = fs::read_to_string(&manifest).unwrap();
    assert!(text.contains("\"schema_version\": 46"));
    let fragment = "\"animation_presets\": [],\n    \"transition_favorites\": [\n      \
                    {\"effect\": \"cross_zoom\", \"duration_frames\": 12},\n      \
                    {\"effect\": \"cut\", \"duration_frames\": 1},\n      \
                    {\"effect\": \"stinger\", \"slot\": 3, \"duration_frames\": 45},\n      \
                    {\"effect\": \"vertical_slide\", \"duration_frames\": 3600}\n    ],\n    \"input_categories\": [],\n    \"input_lifecycle\": [],\n    \"audio_groups\": [";
    assert!(text.contains(fragment), "{text}");

    for (from, to) in [
        // Unknown effects and casing.
        ("\"cross_zoom\"", "\"cross-zoom\""),
        ("\"cut\"", "\"Cut\""),
        // Durations are 1..=3600.
        ("\"duration_frames\": 1}", "\"duration_frames\": 0}"),
        ("\"duration_frames\": 3600}", "\"duration_frames\": 3601}"),
        // Only a Stinger names a slot, and slots are 1 through 8.
        ("\"slot\": 3", "\"slot\": 9"),
        (
            "{\"effect\": \"stinger\", \"slot\": 3, ",
            "{\"effect\": \"stinger\", ",
        ),
        (
            "{\"effect\": \"cut\", ",
            "{\"effect\": \"cut\", \"slot\": 1, ",
        ),
        // Exactly four favourites.
        ("{\"effect\": \"cut\", \"duration_frames\": 1},\n      ", ""),
        (
            "{\"effect\": \"cut\", \"duration_frames\": 1},",
            "{\"effect\": \"cut\", \"duration_frames\": 1}, {\"effect\": \"cut\", \
             \"duration_frames\": 1},",
        ),
        // The field is required.
        ("\"transition_favorites\"", "\"favorites\""),
    ] {
        assert!(text.contains(from), "{from}");
        fs::write(&manifest, text.replacen(from, to, 1)).unwrap();
        assert!(store.load().is_err(), "{from} -> {to} loaded");
    }
}

#[test]
fn every_overlay_transition_round_trips_as_a_runtime_token() {
    let directory = TestDirectory::new("overlays");
    let store = ProjectStore::new(directory.path("show.freemix")).unwrap();
    let mut overlays = RuntimeOverlays::default();
    for (index, transition) in RuntimeOverlayTransition::ALL
        .into_iter()
        .skip(5)
        .enumerate()
    {
        overlays.desired[index].transition = transition;
        overlays.desired[index].duration_frames = 20;
        overlays.realized[index].transition = transition;
        overlays.realized[index].duration_frames = 20;
    }
    let stored = StoredProject::from_project_with_complete_runtime_state(
        project(),
        RuntimeRouting::default(),
        RuntimeManualTransitions::default(),
        RuntimeFadeToBlack::default(),
        overlays.clone(),
        ProjectPosition::default(),
        Vec::new(),
    )
    .unwrap();
    store.save(&stored).unwrap();
    assert_eq!(store.load().unwrap().runtime_overlays(), &overlays);
    let text = fs::read_to_string(directory.path("show.freemix/project.json")).unwrap();
    for token in [
        "vertical_slide",
        "zoom",
        "fly",
        "fly_rotate",
        "cube",
        "cube_zoom",
        "cross_zoom",
        "merge",
    ] {
        assert!(
            text.contains(&format!("\"transition\": \"{token}\"")),
            "{token}"
        );
    }
    for transition in RuntimeOverlayTransition::ALL {
        assert_eq!(
            RuntimeOverlayTransition::from_token(transition.token()),
            Some(transition)
        );
    }
    assert_eq!(RuntimeOverlayTransition::from_token("stinger"), None);
    assert_eq!(RuntimeOverlayTransition::from_token("alpha_fade"), None);
}

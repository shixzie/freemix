//! The project's animation preset library at schema 34 (GX-011): strict,
//! exact `animation_presets` in `project.json`, sharing the `.fmpreset`
//! body encoding (`fm-preset-file`).

use std::{
    fs,
    num::{NonZeroU32, NonZeroU128},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use fm_model::{
    AnimationPresetId, AnimationPresetLibrary, Project, ProjectSettings, UserAnimationPreset,
    fm_animation::{
        Easing, Keyframe, MotionClip, MotionTarget, Phase, PhaseMotion, Property, Rgba, Timebase,
        Track, TrackKey, Value,
    },
};
use fm_persistence::{ProjectPosition, ProjectStore, RuntimeRouting, StoredProject};
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
            "fm-persistence-presets-{}-{sequence}-{name}",
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

/// A 15-frame slide from x -400 with a fading colour, against 400x100.
fn motion() -> PhaseMotion {
    let track = |property| TrackKey::new(MotionTarget, Phase::In, property);
    let clip = MotionClip::from_tracks(
        Timebase::new(FrameRate::new(30, 1).unwrap()),
        15,
        0,
        vec![
            Track::new(
                track(Property::PositionX),
                vec![
                    Keyframe::new(0, Value::Scalar(-400), Easing::CubicOut),
                    Keyframe::new(15, Value::Scalar(0), Easing::Linear),
                ],
            ),
            Track::new(
                track(Property::Color),
                vec![Keyframe::new(
                    0,
                    Value::Color(Rgba::new(255, 0, 16, 255)),
                    Easing::Step,
                )],
            ),
        ],
    )
    .unwrap();
    PhaseMotion::new(clip, 400, 100).unwrap()
}

const MOTION_BODY: &str = "\"phase\": \"out\", \"timebase\": \"30/1\", \"frames\": 15, \
\"width\": 400, \"height\": 100, \"tracks\": [{\"property\": \"position_x\", \"keys\": \
[{\"frame\": 0, \"value\": -400, \"easing\": \"cubic-out\"}, {\"frame\": 15, \"value\": 0, \
\"easing\": \"linear\"}]}, {\"property\": \"color\", \"keys\": [{\"frame\": 0, \"value\": \
\"#ff0010ff\", \"easing\": \"step\"}]}]";

fn preset(id: u32, name: &str) -> UserAnimationPreset {
    UserAnimationPreset {
        id: AnimationPresetId::new(NonZeroU32::new(id).unwrap()),
        name: name.to_owned(),
        phase: Phase::Out,
        motion: motion(),
    }
}

fn project() -> Project {
    let frame_rate = FrameRate::new(30, 1).unwrap();
    Project::new(
        ProjectId::new(NonZeroU128::new(5).unwrap()),
        "Presets",
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
fn the_library_round_trips_exactly_and_is_strict() {
    let directory = TestDirectory::new("library");
    let store = ProjectStore::new(directory.path("show.freemix")).unwrap();
    let mut expected = project();
    expected.set_animation_presets(
        AnimationPresetLibrary::checked(vec![preset(4, "Swoosh \"fast\""), preset(2, "Drift")])
            .unwrap(),
    );
    store.save(&stored(expected.clone())).unwrap();
    let loaded = store.load().unwrap();
    assert_eq!(
        loaded.project().animation_presets(),
        expected.animation_presets()
    );
    let manifest = directory.path("show.freemix/project.json");
    let text = fs::read_to_string(&manifest).unwrap();
    let fragment = format!(
        "\"animation_presets\": [\n      {{\"id\": 4, \"name\": \"Swoosh \\\"fast\\\"\", \
         {MOTION_BODY}}},\n      {{\"id\": 2, \"name\": \"Drift\", {MOTION_BODY}}}\n    ],\n    \"transition_favorites\": ["
    );
    assert!(text.contains(&fragment), "{text}");

    // Duplicate ids or names, a zero box, an Out-phase track, an unknown
    // key, and a missing key are refused on load.
    for (from, to) in [
        ("{\"id\": 2,", "{\"id\": 4,"),
        ("\"name\": \"Drift\"", "\"name\": \"Swoosh \\\"fast\\\"\""),
        ("\"width\": 400,", "\"width\": 0,"),
        ("\"frames\": 15,", "\"frames\": 15, \"extra\": 1,"),
        ("\"height\": 100, ", ""),
        ("\"easing\": \"step\"", "\"easing\": \"bounce\""),
    ] {
        assert!(text.contains(from), "{from}");
        fs::write(&manifest, text.replacen(from, to, 1)).unwrap();
        assert!(store.load().is_err(), "{from} -> {to} loaded");
    }
}

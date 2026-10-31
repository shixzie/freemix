//! OR-013 at schema 83: highlight reels replace, remove, and restore in
//! display order, and an input a reel clip uses cannot be removed.

use std::num::NonZeroU128;

use fm_model::{
    CURRENT_SCHEMA_VERSION, Input, InputKind, InputReference, Project, ProjectSettings,
    RemoveInputError, SimulatedAudio, SimulatedInput, SimulatedVideo, ValidationErrorKind,
};
use fm_types::reel::{MAX_REELS, ReelClip, ReelSpec, ReelSpecError};
use fm_types::{
    AudioFormat, ChannelLayout, ColorMetadata, FrameRate, InputId, PixelFormat, ProjectId,
    SampleFormat, SampleRate, ScanMode, VideoDimensions, VideoFormat,
};

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
        "Reels",
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
    for value in 1..=3 {
        project.add_input(Input {
            id: input(value),
            name: format!("Camera {value}"),
            kind: InputKind::Simulated(SimulatedInput::new(
                SimulatedVideo::Bars,
                SimulatedAudio::Silence,
            )),
            required_capabilities: Vec::new(),
        });
    }
    project.add_input(Input {
        id: input(9),
        name: "Replay A".to_owned(),
        kind: InputKind::Replay,
        required_capabilities: Vec::new(),
    });
    project
}

fn clip(event: u32, angle: InputId) -> ReelClip {
    ReelClip {
        event,
        angle,
        trim_in_frames: 0,
        trim_out_frames: 0,
        speed_percent: 100,
        reverse: false,
        dissolve_frames: 0,
    }
}

fn reel(id: u64, name: &str, angle: InputId) -> ReelSpec {
    ReelSpec {
        id,
        name: name.to_owned(),
        clips: vec![clip(1, angle)],
        music: None,
    }
}

fn ids(project: &Project) -> Vec<u64> {
    project.reels().iter().map(|reel| reel.id).collect()
}

#[test]
fn reels_replace_remove_and_restore_in_order() {
    assert_eq!(CURRENT_SCHEMA_VERSION.get(), 102);
    let mut project = project();
    assert!(project.reels().is_empty());
    let second = reel(2, "Second", input(1));
    let first = reel(1, "First", input(2));
    assert_eq!(project.set_reel(second.clone()), Ok(None));
    assert_eq!(project.set_reel(first.clone()), Ok(None));
    assert_eq!(ids(&project), [2, 1]);
    let replacement = reel(2, "Replacement", input(3));
    assert_eq!(project.set_reel(replacement.clone()), Ok(Some(second)));
    assert_eq!(ids(&project), [2, 1]);
    assert_eq!(project.reels()[0], replacement);
    assert_eq!(project.remove_reel(2), Some(replacement));
    assert_eq!(ids(&project), [1]);
    assert_eq!(project.remove_reel(2), None);
    let restored = vec![reel(3, "Third", input(1)), first];
    project.restore_reels(restored.clone());
    assert_eq!(project.reels(), restored.as_slice());
    for id in 4..=u64::try_from(MAX_REELS + 1).unwrap() {
        assert_eq!(
            project.set_reel(reel(id, "Another", input(1))),
            Ok(None),
            "{id}"
        );
    }
    assert_eq!(
        project.set_reel(reel(10, "Too many", input(1))),
        Err(ReelSpecError::TooManyReels)
    );
    assert_eq!(project.reels().len(), MAX_REELS);
}

#[test]
fn a_reel_angle_keeps_its_input_from_being_removed() {
    let mut project = project();
    project.restore_reels(vec![reel(5, "Highlights", input(1))]);
    assert_eq!(
        project.input_reference(input(1)),
        Some(InputReference::ReelClip { reel: 5 })
    );
    assert_eq!(
        project.remove_input(input(1)),
        Err(RemoveInputError::DomainReference(input(1)))
    );
    assert_eq!(project.input_reference(input(3)), None);
    project.remove_input(input(3)).unwrap();
    // Clearing the reel frees the angle.
    assert!(project.remove_reel(5).is_some());
    project.remove_input(input(1)).unwrap();
    assert_eq!(project.validate(), Ok(()));
}

#[test]
fn reels_are_checked_and_duplicate_ids_are_reported_by_validation() {
    let mut project = project();
    for (spec, error) in [
        (
            ReelSpec {
                clips: Vec::new(),
                ..reel(1, "Empty", input(1))
            },
            ReelSpecError::EmptyReel,
        ),
        (
            ReelSpec {
                clips: vec![clip(1, input(9))],
                ..reel(1, "Replay", input(1))
            },
            ReelSpecError::UnrecordedAngle(input(9)),
        ),
        (
            ReelSpec {
                clips: vec![clip(1, input(7))],
                ..reel(1, "Unknown", input(1))
            },
            ReelSpecError::UnrecordedAngle(input(7)),
        ),
        (
            ReelSpec {
                clips: vec![ReelClip {
                    speed_percent: 60,
                    ..clip(1, input(1))
                }],
                ..reel(1, "Fast", input(1))
            },
            ReelSpecError::Speed(60),
        ),
        (
            ReelSpec {
                name: "  ".to_owned(),
                ..reel(1, "Blank", input(1))
            },
            ReelSpecError::Name,
        ),
    ] {
        assert_eq!(project.set_reel(spec), Err(error));
    }
    assert!(project.reels().is_empty());
    project.restore_reels(vec![reel(1, "One", input(1)), reel(1, "Again", input(2))]);
    let errors = project.validate().unwrap_err();
    assert!(
        errors.iter().any(
            |error| error.field == "reels.id" && error.kind == ValidationErrorKind::DuplicateId
        ),
        "{errors:?}"
    );
}

//! IN-003 media playback settings (schema 36): bounded, validated with the
//! project, and replaced only on media inputs.

use std::num::NonZeroU128;

use fm_model::{
    CURRENT_SCHEMA_VERSION, Input, InputKind, MAX_MEDIA_SPEED_BASIS_POINTS,
    MEDIA_SPEED_NORMAL_BASIS_POINTS, MIN_MEDIA_SPEED_BASIS_POINTS, MediaAudioSpeedPolicy,
    MediaEndAction, MediaGrowing, MediaPlaybackSettings, MediaPlaybackSettingsError, Project,
    ProjectSettings, SetMediaPlaybackError,
};
use fm_types::{
    AudioFormat, ChannelLayout, ColorMetadata, FrameRate, InputId, PixelFormat, ProjectId,
    SampleFormat, SampleRate, ScanMode, VideoDimensions, VideoFormat,
};

fn input_id(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn project() -> Project {
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
    project.add_input(Input {
        id: input_id(1),
        name: "Clip".into(),
        kind: InputKind::Media {
            asset_uri: "asset://media/clip.mov".into(),
            playback: MediaPlaybackSettings::default(),
        },
        required_capabilities: Vec::new(),
    });
    project.add_input(Input {
        id: input_id(2),
        name: "Color".into(),
        kind: InputKind::Color,
        required_capabilities: Vec::new(),
    });
    project
}

#[test]
fn defaults_play_the_whole_file_once_at_normal_speed() {
    assert_eq!(CURRENT_SCHEMA_VERSION.get(), 97);
    let settings = MediaPlaybackSettings::default();
    assert_eq!(settings.trim_in, 0);
    assert_eq!(settings.trim_out, None);
    assert_eq!(settings.end_action, MediaEndAction::Hold);
    assert_eq!(settings.end_trigger_lead, None);
    assert_eq!(settings.speed_basis_points, MEDIA_SPEED_NORMAL_BASIS_POINTS);
    assert_eq!(settings.audio_policy, MediaAudioSpeedPolicy::Mute);
    assert_eq!(settings.audio_stream, None);
    assert_eq!(settings.growing, None);
    assert_eq!(settings.start_frame, 0);
    assert!(settings.validate().is_ok());
    for action in [
        MediaEndAction::Stop,
        MediaEndAction::Hold,
        MediaEndAction::Loop,
    ] {
        assert_eq!(MediaEndAction::from_token(action.token()), Some(action));
    }
}

#[test]
fn every_bound_is_enforced() {
    let base = MediaPlaybackSettings::default();
    let cases = [
        (
            MediaPlaybackSettings {
                trim_in: 10,
                trim_out: Some(10),
                start_frame: 10,
                ..base
            },
            MediaPlaybackSettingsError::InvalidTrim,
        ),
        (
            MediaPlaybackSettings {
                trim_in: 10,
                start_frame: 9,
                ..base
            },
            MediaPlaybackSettingsError::StartOutsideTrim,
        ),
        (
            MediaPlaybackSettings {
                trim_out: Some(20),
                start_frame: 20,
                ..base
            },
            MediaPlaybackSettingsError::StartOutsideTrim,
        ),
        (
            MediaPlaybackSettings {
                speed_basis_points: MIN_MEDIA_SPEED_BASIS_POINTS - 1,
                ..base
            },
            MediaPlaybackSettingsError::SpeedOutOfRange,
        ),
        (
            MediaPlaybackSettings {
                speed_basis_points: MAX_MEDIA_SPEED_BASIS_POINTS + 1,
                ..base
            },
            MediaPlaybackSettingsError::SpeedOutOfRange,
        ),
        (
            MediaPlaybackSettings {
                end_trigger_lead: Some(3_601),
                ..base
            },
            MediaPlaybackSettingsError::EndTriggerLeadOutOfRange,
        ),
        (
            MediaPlaybackSettings {
                growing: Some(MediaGrowing {
                    live_edge_offset_frames: 0,
                    idle_completion_refreshes: 0,
                }),
                ..base
            },
            MediaPlaybackSettingsError::LiveEdgeOffsetOutOfRange,
        ),
        (
            MediaPlaybackSettings {
                growing: Some(MediaGrowing {
                    live_edge_offset_frames: 2,
                    idle_completion_refreshes: 100_001,
                }),
                ..base
            },
            MediaPlaybackSettingsError::IdleCompletionOutOfRange,
        ),
    ];
    for (settings, error) in cases {
        assert_eq!(settings.validate(), Err(error), "{settings:?}");
    }
}

#[test]
fn a_new_trim_pulls_the_start_frame_inside() {
    let settings = MediaPlaybackSettings {
        start_frame: 5,
        ..MediaPlaybackSettings::default()
    };
    let trimmed = settings.with_trim(10, Some(100)).unwrap();
    assert_eq!((trimmed.trim_in, trimmed.trim_out), (10, Some(100)));
    assert_eq!(trimmed.start_frame, 10);
    let kept = MediaPlaybackSettings {
        start_frame: 50,
        ..trimmed
    }
    .with_trim(20, None)
    .unwrap();
    assert_eq!(kept.start_frame, 50);
    assert_eq!(
        settings.with_trim(10, Some(9)),
        Err(MediaPlaybackSettingsError::InvalidTrim)
    );
}

#[test]
fn only_media_inputs_take_settings_and_the_project_validates_them() {
    let mut project = project();
    let settings = MediaPlaybackSettings {
        trim_in: 3,
        trim_out: Some(90),
        end_action: MediaEndAction::Loop,
        start_frame: 3,
        ..MediaPlaybackSettings::default()
    };
    project.set_media_playback(input_id(1), settings).unwrap();
    assert_eq!(project.media_playback(input_id(1)), Some(&settings));
    assert_eq!(project.media_playback(input_id(2)), None);
    assert_eq!(
        project.set_media_playback(input_id(2), settings),
        Err(SetMediaPlaybackError::NotMediaInput(input_id(2)))
    );
    assert_eq!(
        project.set_media_playback(input_id(9), settings),
        Err(SetMediaPlaybackError::UnknownInput(input_id(9)))
    );
    let invalid = MediaPlaybackSettings {
        speed_basis_points: 1,
        ..settings
    };
    assert_eq!(
        project.set_media_playback(input_id(1), invalid),
        Err(SetMediaPlaybackError::Invalid(
            MediaPlaybackSettingsError::SpeedOutOfRange
        ))
    );
    assert_eq!(project.media_playback(input_id(1)), Some(&settings));
    assert!(project.validate().is_ok());

    // A project assembled with invalid settings fails validation on the
    // offending field.
    let mut broken = self::project();
    broken.add_input(Input {
        id: input_id(3),
        name: "Broken".into(),
        kind: InputKind::Media {
            asset_uri: "asset://media/broken.mov".into(),
            playback: invalid,
        },
        required_capabilities: Vec::new(),
    });
    let error = broken.validate().unwrap_err();
    assert!(
        format!("{error:?}").contains("kind.media.speed_basis_points"),
        "{error:?}"
    );
}

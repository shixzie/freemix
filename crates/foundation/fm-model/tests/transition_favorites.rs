//! SW-002 favourite transition buttons (schema 35): four slots, valid by
//! construction, with stable effect tokens.

use std::num::NonZeroU128;

use fm_model::{
    CURRENT_SCHEMA_VERSION, MAX_TRANSITION_FAVORITE_DURATION_FRAMES, Project, ProjectSettings,
    StingerSlotNumber, TRANSITION_FAVORITE_COUNT, TransitionEffect, TransitionFavorite,
    TransitionFavoriteDurationError, TransitionFavorites,
};
use fm_types::{
    AudioFormat, ChannelLayout, ColorMetadata, FrameRate, PixelFormat, ProjectId, SampleFormat,
    SampleRate, ScanMode, VideoDimensions, VideoFormat,
};

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

#[test]
fn a_new_project_has_four_default_favourites() {
    assert_eq!(CURRENT_SCHEMA_VERSION.get(), 90);
    let project = project();
    let favorites = project.transition_favorites();
    assert_eq!(TRANSITION_FAVORITE_COUNT, 4);
    assert_eq!(
        favorites
            .slots()
            .map(|favorite| (favorite.effect(), favorite.duration_frames())),
        [
            (TransitionEffect::Fade, 30),
            (TransitionEffect::Wipe, 30),
            (TransitionEffect::Fly, 30),
            (TransitionEffect::Cube, 30),
        ]
    );
    assert!(project.validate().is_ok());
}

#[test]
fn favourite_durations_are_bounded_and_slots_are_one_based() {
    assert_eq!(
        TransitionFavorite::new(TransitionEffect::Merge, 0),
        Err(TransitionFavoriteDurationError { duration_frames: 0 })
    );
    assert!(
        TransitionFavorite::new(
            TransitionEffect::Merge,
            MAX_TRANSITION_FAVORITE_DURATION_FRAMES + 1
        )
        .is_err()
    );
    let stinger = TransitionFavorite::new(
        TransitionEffect::Stinger(StingerSlotNumber::new(2).unwrap()),
        MAX_TRANSITION_FAVORITE_DURATION_FRAMES,
    )
    .unwrap();
    let mut favorites = TransitionFavorites::default();
    assert_eq!(favorites.set(0, stinger), None);
    assert_eq!(favorites.set(5, stinger), None);
    assert_eq!(favorites, TransitionFavorites::default());
    let previous = favorites.set(4, stinger).unwrap();
    assert_eq!(previous.effect(), TransitionEffect::Cube);
    assert_eq!(favorites.get(4), Some(stinger));
    assert_eq!(favorites.get(0), None);

    let mut project = project();
    project.set_transition_favorites(favorites);
    assert_eq!(project.transition_favorites().get(4), Some(stinger));
}

#[test]
fn built_in_effect_tokens_round_trip() {
    for effect in TransitionEffect::BUILT_IN {
        let token = effect.token().unwrap();
        assert_eq!(TransitionEffect::from_token(token), Some(effect));
    }
    assert_eq!(
        TransitionEffect::Stinger(StingerSlotNumber::new(1).unwrap()).token(),
        None
    );
    assert_eq!(TransitionEffect::from_token("stinger"), None);
    assert_eq!(TransitionEffect::from_token("Fade"), None);
}

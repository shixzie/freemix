//! IN-011 screen capture audio: a screen or window capture input captures
//! no sound, the whole system output, or (a window only) the sound of the
//! application owning the window. The project validates the choice, and a
//! replaced source carries it.

use std::num::NonZeroU128;

use fm_model::{
    EntityRef, Input, InputKind, InputSourceError, InputSourceSpec, Project, ProjectSettings,
    ValidationErrorKind,
};
use fm_types::screen_capture::{
    ScreenCaptureAudio, ScreenCaptureConfig, ScreenCaptureTarget, WindowRule,
};
use fm_types::{
    AudioFormat, ChannelLayout, ColorMetadata, FrameRate, InputId, PixelFormat, ProjectId,
    SampleFormat, SampleRate, ScanMode, VideoDimensions, VideoFormat,
};

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn project() -> Project {
    let frame_rate = FrameRate::new(30, 1).unwrap();
    Project::new(
        ProjectId::new(NonZeroU128::new(1).unwrap()),
        "Captures",
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

fn capture(id: u128, config: ScreenCaptureConfig) -> Input {
    Input {
        id: input(id),
        name: format!("Capture {id}"),
        kind: InputKind::ScreenCapture(config),
        required_capabilities: Vec::new(),
    }
}

fn window(audio: ScreenCaptureAudio) -> ScreenCaptureConfig {
    ScreenCaptureConfig {
        audio,
        ..ScreenCaptureConfig::new(ScreenCaptureTarget::Window(WindowRule::exact(
            "Firefox", "Video", None,
        )))
    }
}

fn screen(audio: ScreenCaptureAudio) -> ScreenCaptureConfig {
    ScreenCaptureConfig {
        audio,
        ..ScreenCaptureConfig::new(ScreenCaptureTarget::Screen { index: 0 })
    }
}

#[test]
fn a_new_capture_is_silent_and_every_audio_fits_a_window() {
    assert_eq!(
        ScreenCaptureConfig::default().audio,
        ScreenCaptureAudio::None
    );
    let mut project = project();
    for (id, audio) in (1..).zip(ScreenCaptureAudio::ALL) {
        project.add_input(capture(id, window(audio)));
    }
    project.add_input(capture(4, screen(ScreenCaptureAudio::System)));
    project.add_input(capture(5, screen(ScreenCaptureAudio::None)));
    assert_eq!(project.validate(), Ok(()));
}

#[test]
fn a_screen_never_takes_an_application_audio() {
    let mut project = project();
    project.add_input(capture(1, screen(ScreenCaptureAudio::Application)));
    let errors = project.validate().unwrap_err();
    assert!(
        errors.iter().any(|error| {
            error.entity == Some(EntityRef::Input(input(1)))
                && error.field == "kind.screen_capture.audio"
                && error.kind == ValidationErrorKind::OutOfRange
        }),
        "{errors:?}"
    );
    assert_eq!(
        InputSourceSpec::ScreenCapture(screen(ScreenCaptureAudio::Application)).validate(),
        Err(InputSourceError::InvalidScreenCapture)
    );
}

#[test]
fn a_replaced_source_carries_its_audio() {
    let mut project = project();
    project.add_input(capture(1, window(ScreenCaptureAudio::None)));
    let replaced = InputKind::ScreenCapture(window(ScreenCaptureAudio::Application));
    project
        .replace_input_source(input(1), replaced.clone(), Vec::new())
        .unwrap();
    assert_eq!(project.inputs()[0].kind, replaced);
    assert_eq!(
        InputSourceSpec::from_kind(&replaced),
        Some(InputSourceSpec::ScreenCapture(window(
            ScreenCaptureAudio::Application
        )))
    );
}

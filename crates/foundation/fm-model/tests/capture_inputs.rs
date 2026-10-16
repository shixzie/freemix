//! IN-001 local capture inputs (schema 44): the stable key and settings
//! validation, the per-project input limit, the capture settings edit, and
//! the replaceable device source.

use std::num::NonZeroU128;

use fm_model::{
    EntityRef, Input, InputKind, InputSourceError, InputSourceSpec, Project, ProjectSettings,
    SetCaptureSettingsError, ValidationErrorKind,
};
use fm_types::capture::{
    CaptureAudio, CaptureError, CaptureFormat, CaptureLossSlate, CapturePixelFormat, CaptureScan,
    CaptureSettings, MAX_CAPTURE_INPUTS, MAX_CAPTURE_KEY_BYTES,
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
        "Cameras",
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

fn device(id: u128, stable_key: &str, capture: CaptureSettings) -> Input {
    Input {
        id: input(id),
        name: format!("Camera {id}"),
        kind: InputKind::Device {
            stable_key: stable_key.to_owned(),
            capture,
        },
        required_capabilities: Vec::new(),
    }
}

fn hd_settings() -> CaptureSettings {
    CaptureSettings {
        format: Some(CaptureFormat {
            width: 1920,
            height: 1080,
            rate: FrameRate::new(30_000, 1_001).unwrap(),
            pixel_format: CapturePixelFormat::Yuyv,
            scan: CaptureScan::InterlacedTff,
        }),
        audio: CaptureAudio::Alsa("hw:CARD=C920,DEV=0".into()),
        loss: CaptureLossSlate::Black,
    }
}

#[test]
fn project_validation_checks_the_stable_key_settings_and_input_count() {
    let mut project = project();
    project.add_input(device(1, "v4l2:usb-1#Cam", hd_settings()));
    assert_eq!(project.validate(), Ok(()));

    let mut bad = project.clone();
    bad.add_input(device(2, " ", CaptureSettings::default()));
    bad.add_input(device(
        3,
        &"k".repeat(MAX_CAPTURE_KEY_BYTES + 1),
        CaptureSettings::default(),
    ));
    let mut bad_format = hd_settings();
    bad_format.format.as_mut().unwrap().width = 8;
    bad.add_input(device(4, "cam-4", bad_format));
    bad.add_input(device(
        5,
        "cam-5",
        CaptureSettings {
            audio: CaptureAudio::Alsa(String::new()),
            ..CaptureSettings::default()
        },
    ));
    let errors = bad.validate().unwrap_err();
    for (id, field, kind) in [
        (2, "stable_key", ValidationErrorKind::Empty),
        (3, "stable_key", ValidationErrorKind::OutOfRange),
        (
            4,
            "kind.device.capture.format",
            ValidationErrorKind::OutOfRange,
        ),
        (
            5,
            "kind.device.capture.audio",
            ValidationErrorKind::OutOfRange,
        ),
    ] {
        assert!(
            errors
                .iter()
                .any(|error| error.entity == Some(EntityRef::Input(input(id)))
                    && error.field == field
                    && error.kind == kind),
            "{id} {field}: {errors:?}"
        );
    }

    let mut project = self::project();
    for id in 1..=u128::try_from(MAX_CAPTURE_INPUTS).unwrap() {
        project.add_input(device(id, &format!("cam-{id}"), CaptureSettings::default()));
    }
    assert_eq!(project.validate(), Ok(()));
    let extra = u128::try_from(MAX_CAPTURE_INPUTS).unwrap() + 1;
    project.add_input(device(extra, "cam-extra", CaptureSettings::default()));
    let errors = project.validate().unwrap_err();
    assert!(
        errors.iter().any(|error| error.field == "kind.device"
            && error.entity == Some(EntityRef::Input(input(extra)))
            && error.kind == ValidationErrorKind::OutOfRange),
        "{errors:?}"
    );
}

#[test]
fn capture_settings_edit_refuses_unknown_non_device_and_invalid_settings() {
    let mut project = project();
    project.add_input(device(1, "cam-1", CaptureSettings::default()));
    project.add_input(Input {
        id: input(2),
        name: "Color".into(),
        kind: InputKind::Color,
        required_capabilities: Vec::new(),
    });
    project
        .set_capture_settings(input(1), hd_settings())
        .unwrap();
    assert_eq!(
        project.capture_input(input(1)),
        Some(("cam-1", &hd_settings()))
    );
    assert_eq!(
        project.set_capture_settings(input(9), hd_settings()),
        Err(SetCaptureSettingsError::UnknownInput(input(9)))
    );
    assert_eq!(
        project.set_capture_settings(input(2), hd_settings()),
        Err(SetCaptureSettingsError::NotCaptureInput(input(2)))
    );
    let invalid = CaptureSettings {
        audio: CaptureAudio::Alsa("hw 0".into()),
        ..CaptureSettings::default()
    };
    assert_eq!(
        project.set_capture_settings(input(1), invalid),
        Err(SetCaptureSettingsError::Invalid(CaptureError::AlsaDevice))
    );
    assert_eq!(
        project.capture_input(input(1)),
        Some(("cam-1", &hd_settings())),
        "nothing changes on error"
    );
    assert_eq!(project.capture_input(input(2)), None);
}

#[test]
fn a_device_source_is_replaceable_with_default_settings() {
    let spec = InputSourceSpec::Device {
        stable_key: "v4l2:usb-1#Cam".into(),
    };
    assert_eq!(spec.validate(), Ok(()));
    let kind = spec.to_kind(fm_model::MediaPlaybackSettings::default());
    assert_eq!(
        kind,
        InputKind::Device {
            stable_key: "v4l2:usb-1#Cam".into(),
            capture: CaptureSettings::default(),
        }
    );
    let configured = InputKind::Device {
        stable_key: "v4l2:usb-1#Cam".into(),
        capture: hd_settings(),
    };
    assert_eq!(InputSourceSpec::from_kind(&configured), Some(spec));
    assert_eq!(
        InputSourceSpec::Device {
            stable_key: "k".repeat(MAX_CAPTURE_KEY_BYTES + 1)
        }
        .validate(),
        Err(InputSourceError::InvalidLocator)
    );
}

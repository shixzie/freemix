//! AU-001 and SW-004 at schema 39: strip labels, pan, VCA-style groups, and
//! the Fade-to-Black output selection in the project model.

use std::num::NonZeroU128;

use fm_model::{
    AUDIO_GROUP_COUNT, AudioBus, AudioGroup, AudioGroupNumber, AudioLabelError, AudioSoloMode,
    CURRENT_SCHEMA_VERSION, EntityRef, Input, InputAudioStripState, InputGainMilliDb, InputKind,
    InputPanBasisPoints, MAX_AUDIO_LABEL_BYTES, Output, Project, ProjectSettings, Rgba8, Scene,
    SetAudioMixerError, SimulatedAudio, SimulatedInput, SimulatedVideo, SolidColor, StartupPolicy,
    ValidationErrorKind, validate_audio_group_name, validate_audio_strip_label,
};
use fm_types::{
    AudioFormat, BusId, ChannelLayout, ColorMetadata, FrameRate, InputId, OutputId, PixelFormat,
    ProjectId, SampleFormat, SampleRate, ScanMode, SceneId, VideoDimensions, VideoFormat,
};

fn id(value: u128) -> NonZeroU128 {
    NonZeroU128::new(value).unwrap()
}

fn input(value: u128) -> InputId {
    InputId::new(id(value))
}

fn output(value: u128) -> OutputId {
    OutputId::new(id(value))
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
            id: input(value),
            name: name.into(),
            kind: InputKind::Simulated(SimulatedInput::new(
                SimulatedVideo::Solid(SolidColor::new(1, 2, 3, 255)),
                SimulatedAudio::Silence,
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
            id: output(value),
            name: name.into(),
            video_source: SceneId::new(id(20)),
            audio_source: BusId::new(id(30)),
            startup: StartupPolicy::Stopped,
            required_capabilities: Vec::new(),
        });
    }
    project
}

#[test]
fn schema_39_defaults_center_pan_unlabelled_ungrouped_and_every_output_fading() {
    assert_eq!(CURRENT_SCHEMA_VERSION.get(), 44);
    let project = project();
    assert_eq!(
        project.input_audio_strip(input(10)).unwrap().pan,
        InputPanBasisPoints::CENTER
    );
    assert_eq!(project.input_audio_label(input(10)), Some(""));
    assert_eq!(project.input_audio_group(input(10)), None);
    assert_eq!(AUDIO_GROUP_COUNT, 4);
    for (number, group) in project.audio_groups().iter() {
        assert_eq!(group.name, format!("Group {}", number.get()));
        assert_eq!(group.gain, InputGainMilliDb::UNITY);
        assert!(!group.muted);
        assert!(!group.soloed);
        assert_eq!(group.parent, None);
    }
    assert_eq!(project.audio_solo_mode(), AudioSoloMode::InPlace);
    assert!(project.fade_to_black_excluded_outputs().is_empty());
    assert!(project.fade_to_black_affects(output(40)));
    assert_eq!(project.validate(), Ok(()));
}

#[test]
fn pan_is_bounded_like_balance() {
    assert_eq!(InputPanBasisPoints::new(-10_000).unwrap().get(), -10_000);
    assert_eq!(InputPanBasisPoints::new(10_000).unwrap().get(), 10_000);
    assert_eq!(InputPanBasisPoints::new(10_001), None);
    assert_eq!(InputPanBasisPoints::new(-10_001), None);
    assert_eq!(
        InputAudioStripState::default().pan,
        InputPanBasisPoints::CENTER
    );
}

#[test]
fn labels_and_group_names_are_bounded_text() {
    assert_eq!(validate_audio_strip_label(""), Ok(()));
    assert_eq!(validate_audio_strip_label("Host mic"), Ok(()));
    assert_eq!(
        validate_audio_strip_label(&"x".repeat(MAX_AUDIO_LABEL_BYTES)),
        Ok(())
    );
    assert_eq!(
        validate_audio_strip_label(&"x".repeat(MAX_AUDIO_LABEL_BYTES + 1)),
        Err(AudioLabelError::TooLong)
    );
    assert_eq!(
        validate_audio_strip_label("line\nbreak"),
        Err(AudioLabelError::ControlCharacter)
    );
    assert_eq!(validate_audio_group_name("  "), Err(AudioLabelError::Empty));
    assert_eq!(validate_audio_group_name("Hosts"), Ok(()));

    let mut project = project();
    project
        .set_input_audio_label(input(10), "Host mic".into())
        .unwrap();
    assert_eq!(project.input_audio_label(input(10)), Some("Host mic"));
    assert_eq!(
        project.set_input_audio_label(input(10), "\t".into()),
        Err(SetAudioMixerError::Label(AudioLabelError::ControlCharacter))
    );
    assert_eq!(project.input_audio_label(input(10)), Some("Host mic"));
    assert_eq!(
        project.set_input_audio_label(input(99), String::new()),
        Err(SetAudioMixerError::UnknownInput(input(99)))
    );
    let hosts = AudioGroupNumber::new(1).unwrap();
    assert_eq!(
        project.set_audio_group(
            hosts,
            AudioGroup {
                name: String::new(),
                gain: InputGainMilliDb::UNITY,
                muted: false,
                soloed: false,
                parent: None,
            },
        ),
        Err(SetAudioMixerError::Label(AudioLabelError::Empty))
    );
    assert_eq!(project.audio_groups().get(hosts).name, "Group 1");
}

#[test]
fn groups_are_numbered_and_membership_follows_duplicates_but_labels_do_not() {
    assert_eq!(AudioGroupNumber::new(0), None);
    assert_eq!(AudioGroupNumber::new(5), None);
    let group = AudioGroupNumber::new(3).unwrap();
    let mut project = project();
    assert!(project.set_input_audio_group(input(10), Some(group)));
    assert!(!project.set_input_audio_group(input(99), Some(group)));
    project
        .set_input_audio_label(input(10), "Host mic".into())
        .unwrap();
    project
        .duplicate_input(input(10), input(12), "Host copy".into())
        .unwrap();
    assert_eq!(project.input_audio_group(input(12)), Some(group));
    assert_eq!(project.input_audio_label(input(12)), Some(""));
    project.remove_input(input(12)).unwrap();
    assert_eq!(project.input_audio_group(input(12)), None);
}

#[test]
fn fade_to_black_output_selection_tracks_outputs_and_is_validated() {
    let mut project = project();
    project.set_fade_to_black_output(output(41), false);
    assert!(!project.fade_to_black_affects(output(41)));
    assert!(project.fade_to_black_affects(output(40)));
    assert_eq!(project.validate(), Ok(()));
    project.set_fade_to_black_output(output(41), true);
    assert!(project.fade_to_black_excluded_outputs().is_empty());

    project.set_fade_to_black_output(output(41), false);
    project.remove_output(output(41)).unwrap();
    assert!(project.fade_to_black_excluded_outputs().is_empty());

    project.set_fade_to_black_output(output(99), false);
    let errors = project.validate().unwrap_err();
    assert!(
        errors
            .iter()
            .any(|error| error.field == "fade_to_black_excluded_outputs"
                && error.kind
                    == ValidationErrorKind::MissingReference(EntityRef::Output(output(99))))
    );
}

#[test]
fn restored_unusable_labels_and_names_fail_validation() {
    let mut project = project();
    project.restore_input_audio_label(input(10), "x".repeat(MAX_AUDIO_LABEL_BYTES + 1));
    let mut groups = project.audio_groups().clone();
    groups.set(
        AudioGroupNumber::new(4).unwrap(),
        AudioGroup {
            name: " ".into(),
            gain: InputGainMilliDb::UNITY,
            muted: false,
            soloed: false,
            parent: None,
        },
    );
    project.restore_audio_groups(groups);
    let errors = project.validate().unwrap_err();
    assert!(
        errors
            .iter()
            .any(|error| error.field == "audio_strip.label")
    );
    assert!(errors.iter().any(
        |error| error.field == "audio_groups.name" && error.kind == ValidationErrorKind::Empty
    ));
}

fn group(number: u8) -> AudioGroupNumber {
    AudioGroupNumber::new(number).unwrap()
}

fn nested(project: &Project, number: u8, parent: Option<u8>) -> AudioGroup {
    AudioGroup {
        parent: parent.map(group),
        ..project.audio_groups().get(group(number)).clone()
    }
}

#[test]
fn schema_42_solo_mode_tokens_round_trip() {
    for (mode, token) in [
        (AudioSoloMode::InPlace, "sip"),
        (AudioSoloMode::Pfl, "pfl"),
        (AudioSoloMode::Afl, "afl"),
    ] {
        assert_eq!(mode.token(), token);
        assert_eq!(AudioSoloMode::from_token(token), Some(mode));
    }
    assert_eq!(AudioSoloMode::from_token("PFL"), None);
    let mut project = project();
    project.set_audio_solo_mode(AudioSoloMode::Afl);
    assert_eq!(project.audio_solo_mode(), AudioSoloMode::Afl);
    assert_eq!(project.validate(), Ok(()));
}

#[test]
fn group_parents_nest_without_cycles() {
    let mut project = project();
    for (number, parent) in [(1, 2), (2, 3), (3, 4)] {
        let value = nested(&project, number, Some(parent));
        project.set_audio_group(group(number), value).unwrap();
    }
    assert_eq!(
        project
            .audio_groups()
            .chain(Some(group(1)))
            .map(AudioGroupNumber::get)
            .collect::<Vec<_>>(),
        [1, 2, 3, 4]
    );
    for (number, parent) in [(4, 1), (4, 4), (2, 1)] {
        let value = nested(&project, number, Some(parent));
        assert_eq!(
            project.set_audio_group(group(number), value),
            Err(SetAudioMixerError::GroupCycle(group(number)))
        );
    }
    assert_eq!(project.audio_groups().get(group(4)).parent, None);
    assert_eq!(project.validate(), Ok(()));

    // A soloed group is plain state on the group.
    let soloed = AudioGroup {
        soloed: true,
        ..project.audio_groups().get(group(2)).clone()
    };
    project.set_audio_group(group(2), soloed).unwrap();
    assert!(project.audio_groups().get(group(2)).soloed);
}

#[test]
fn restored_cyclic_group_parents_fail_validation() {
    let mut project = project();
    let mut groups = project.audio_groups().clone();
    groups.set(group(1), nested(&project, 1, Some(2)));
    groups.set(group(2), nested(&project, 2, Some(1)));
    project.restore_audio_groups(groups);
    assert!(!project.audio_groups().is_acyclic());
    let errors = project.validate().unwrap_err();
    assert!(
        errors
            .iter()
            .any(|error| error.field == "audio_groups.parent"
                && error.kind == ValidationErrorKind::Cycle)
    );
}

//! OR-011 at schema 40: the project's fullscreen display-output slots are
//! validated configuration (usable selectors and modes, existing source
//! outputs, one display per slot) and pin the outputs they show.

use std::num::NonZeroU128;

use fm_model::{
    AudioBus, CURRENT_SCHEMA_VERSION, EntityRef, Input, InputKind, MAX_DISPLAY_OUTPUTS, Output,
    Project, ProjectSettings, RemoveOutputError, Rgba8, Scene, SetDisplayOutputError,
    SimulatedAudio, SimulatedInput, SimulatedVideo, SolidColor, StartupPolicy, ValidationErrorKind,
};
use fm_types::display::{
    DisplayId, DisplayMode, DisplayOutputConfig, DisplayOutputSlot, DisplayOutputSource,
    DisplaySelector, HdrRequest, ModeRequest,
};
use fm_types::{
    AudioFormat, BusId, ChannelLayout, ColorMetadata, FrameRate, InputId, OutputId, OutputScaling,
    PixelFormat, ProjectId, SampleFormat, SampleRate, ScanMode, SceneId, VideoDimensions,
    VideoFormat,
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
        "Displays",
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

fn slot(value: u8) -> DisplayOutputSlot {
    DisplayOutputSlot::new(value).unwrap()
}

fn config(text: &str) -> DisplayOutputConfig {
    text.parse().unwrap()
}

#[test]
fn schema_40_starts_without_display_outputs_and_bounds_slots() {
    assert_eq!(CURRENT_SCHEMA_VERSION.get(), 87);
    assert_eq!(MAX_DISPLAY_OUTPUTS, 2);
    assert_eq!(DisplayOutputSlot::new(0), None);
    assert_eq!(DisplayOutputSlot::new(3), None);
    let project = project();
    assert!(project.display_outputs().is_empty());
    assert_eq!(project.display_output(slot(1)), None);
    assert_eq!(project.validate(), Ok(()));
}

#[test]
fn slots_configure_replace_and_clear_with_every_field() {
    let mut project = project();
    let first = DisplayOutputConfig {
        display: DisplaySelector::IdOrName {
            id: DisplayId::new("cg:69733382").unwrap(),
            name: "DELL U2720Q".into(),
        },
        source: DisplayOutputSource::Output(output(41)),
        mode: ModeRequest::Exact(DisplayMode::new(1920, 1080, 59_940)),
        scaling: OutputScaling::OneToOne,
        hdr: HdrRequest::Auto,
    };
    assert_eq!(project.set_display_output(slot(1), first.clone()), Ok(None));
    let second = config("display=index:1,source=preview");
    assert_eq!(
        project.set_display_output(slot(2), second.clone()),
        Ok(None)
    );
    assert_eq!(project.display_output(slot(1)), Some(&first));
    assert_eq!(project.validate(), Ok(()));

    // Moving slot 2 to a different display returns what it held.
    let moved = config("display=name:Wall,source=program,scaling=fill");
    assert_eq!(
        project.set_display_output(slot(2), moved.clone()),
        Ok(Some(second))
    );
    // A slot may keep its own display while other fields change.
    let retuned = DisplayOutputConfig {
        hdr: HdrRequest::Off,
        ..first.clone()
    };
    assert_eq!(
        project.set_display_output(slot(1), retuned.clone()),
        Ok(Some(first))
    );
    assert_eq!(project.clear_display_output(slot(2)), Some(moved));
    assert_eq!(project.clear_display_output(slot(2)), None);
    assert_eq!(
        project
            .display_outputs()
            .keys()
            .copied()
            .collect::<Vec<_>>(),
        vec![slot(1)]
    );
    assert_eq!(project.display_output(slot(1)), Some(&retuned));
}

#[test]
fn refusals_name_the_problem_and_change_nothing() {
    let mut project = project();
    project
        .set_display_output(slot(1), config("display=id:left,source=output:40"))
        .unwrap();
    let before = project.clone();
    let cases = [
        (
            config("display=index:2,source=output:99"),
            SetDisplayOutputError::UnknownOutput(output(99)),
            "unknown_output",
        ),
        (
            config("display=id:left"),
            SetDisplayOutputError::DisplayInUse {
                display: config("display=id:left").display,
                slot: slot(1),
            },
            "display_in_use",
        ),
        (
            DisplayOutputConfig {
                mode: ModeRequest::Exact(DisplayMode::new(0, 1080, 60_000)),
                ..config("display=index:3")
            },
            SetDisplayOutputError::InvalidMode,
            "invalid_mode",
        ),
        (
            DisplayOutputConfig::program_on(DisplaySelector::Index(usize::MAX)),
            SetDisplayOutputError::InvalidDisplay(DisplaySelector::Index(usize::MAX)),
            "invalid_display",
        ),
    ];
    for (refused, error, code) in cases {
        assert_eq!(
            project.set_display_output(slot(2), refused),
            Err(error.clone())
        );
        assert_eq!(error.code(), code);
        assert_eq!(project, before);
    }
    assert_eq!(
        SetDisplayOutputError::UnknownOutput(output(99)).to_string(),
        "output 99 does not exist"
    );
    // Two identical monitors may share a name when their ids differ.
    project
        .set_display_output(
            slot(2),
            DisplayOutputConfig::program_on(DisplaySelector::IdOrName {
                id: DisplayId::new("right").unwrap(),
                name: "left".into(),
            }),
        )
        .unwrap();
}

#[test]
fn a_shown_output_cannot_be_removed_until_its_slot_moves_on() {
    let mut project = project();
    project
        .set_display_output(slot(2), config("display=index:0,source=output:41"))
        .unwrap();
    assert_eq!(
        project.remove_output(output(41)),
        Err(RemoveOutputError::DisplayOutputReference {
            output: output(41),
            slot: slot(2),
        })
    );
    project
        .set_display_output(slot(2), config("display=index:0,source=preview"))
        .unwrap();
    assert_eq!(project.remove_output(output(41)), Ok(()));
}

#[test]
fn validation_reports_restored_slots_that_break_the_rules() {
    let mut project = project();
    project.restore_display_output(slot(1), config("display=name:Wall,source=output:77"));
    project.restore_display_output(slot(2), config("display=name:Wall"));
    let errors = project.validate().unwrap_err();
    let kinds = errors
        .iter()
        .map(|error| (error.field, error.kind))
        .collect::<Vec<_>>();
    assert!(
        kinds.contains(&(
            "display_outputs.source",
            ValidationErrorKind::MissingReference(EntityRef::Output(output(77)))
        )),
        "{kinds:?}"
    );
    assert!(
        kinds.contains(&(
            "display_outputs.display",
            ValidationErrorKind::DuplicateReference
        )),
        "{kinds:?}"
    );
}

/// OR-017 (schema 53): the `LiveLAN` configuration starts disabled on Program,
/// is checked against the project's outputs, pins the output it streams,
/// and is reported by validation when restored broken.
#[test]
fn live_lan_config_is_checked_and_pins_its_output() {
    use fm_types::live_lan::{
        DEFAULT_LIVE_LAN_VIEWERS, LiveLanConfig, LiveLanConfigError, LiveLanPin, LiveLanSource,
    };
    let mut project = project();
    assert_eq!(project.live_lan(), &LiveLanConfig::default());
    assert!(!project.live_lan().enabled);
    assert_eq!(project.live_lan().max_viewers, DEFAULT_LIVE_LAN_VIEWERS);

    let before = project.clone();
    let unknown = LiveLanConfig {
        enabled: true,
        source: LiveLanSource::Output(output(99)),
        ..LiveLanConfig::default()
    };
    assert_eq!(
        project.set_live_lan(unknown),
        Err(LiveLanConfigError::UnknownOutput(output(99)))
    );
    let crowded = LiveLanConfig {
        max_viewers: 0,
        ..LiveLanConfig::default()
    };
    assert_eq!(
        project.set_live_lan(crowded),
        Err(LiveLanConfigError::MaxViewers)
    );
    assert_eq!(project, before);

    let clean = LiveLanConfig {
        enabled: true,
        source: LiveLanSource::Output(output(41)),
        max_viewers: 4,
        pin: Some(LiveLanPin::parse("2468").unwrap()),
    };
    assert_eq!(
        project.set_live_lan(clean.clone()),
        Ok(LiveLanConfig::default())
    );
    assert_eq!(project.live_lan(), &clean);
    assert_eq!(project.validate(), Ok(()));
    assert_eq!(
        project.remove_output(output(41)),
        Err(RemoveOutputError::LiveLanReference { output: output(41) })
    );
    project.set_live_lan(LiveLanConfig::default()).unwrap();
    assert_eq!(project.remove_output(output(41)), Ok(()));

    project.restore_live_lan(LiveLanConfig {
        source: LiveLanSource::Output(output(77)),
        max_viewers: 33,
        ..LiveLanConfig::default()
    });
    let kinds = project
        .validate()
        .unwrap_err()
        .iter()
        .map(|error| (error.field, error.kind))
        .collect::<Vec<_>>();
    assert!(
        kinds.contains(&("live_lan.max_viewers", ValidationErrorKind::OutOfRange)),
        "{kinds:?}"
    );
}

//! IN-026 input lifecycle (schema 37): the category list, colour labels,
//! pause and close flags, duplicate, replaceable sources, and the batch
//! import plan.

use std::num::NonZeroU128;

use fm_model::{
    BatchImportRefusal, BatchImportRequest, GeneratorVideo, Input, InputCategory, InputCategoryId,
    InputColorLabel, InputKind, InputLifecycle, InputLifecycleError, InputSourceError,
    InputSourceSpec, MAX_BATCH_IMPORT_ITEMS, MAX_INPUT_CATEGORIES, MAX_INPUT_CATEGORY_NAME_BYTES,
    MediaPlaybackSettings, Project, ProjectSettings, SimulatedAudio, SimulatedInput,
    SimulatedVideo, SolidColor, is_audio_file_name, is_media_file_name, plan_batch_import,
    unique_input_name, validate_input_categories,
};
use fm_types::{
    AudioFormat, ChannelLayout, ColorMetadata, FrameRate, InputId, PixelFormat, ProjectId,
    SampleFormat, SampleRate, ScanMode, VideoDimensions, VideoFormat,
};

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn category(value: u32) -> InputCategoryId {
    InputCategoryId::new(value).unwrap()
}

fn project() -> Project {
    let frame_rate = FrameRate::new(30, 1).unwrap();
    let mut project = Project::new(
        ProjectId::new(NonZeroU128::new(1).unwrap()),
        "Lifecycle",
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
    for (id, name) in [(1, "Camera"), (2, "Slides")] {
        project.add_input(Input {
            id: input(id),
            name: name.into(),
            kind: InputKind::Simulated(SimulatedInput::new(
                SimulatedVideo::Solid(SolidColor::new(10, 20, 30, 255)),
                SimulatedAudio::Sine { frequency_hz: 440 },
            )),
            required_capabilities: vec!["capture.camera.raw".into()],
        });
    }
    project
}

fn categories() -> Vec<InputCategory> {
    vec![
        InputCategory::new(category(1), "Cameras"),
        InputCategory::new(category(2), "Graphics"),
    ]
}

#[test]
fn category_lists_are_bounded_unique_and_named() {
    assert!(validate_input_categories(&categories()).is_ok());
    assert!(validate_input_categories(&[]).is_ok());
    let error = |list: Vec<InputCategory>| validate_input_categories(&list).unwrap_err();
    assert_eq!(
        error(vec![
            InputCategory::new(category(1), "A"),
            InputCategory::new(category(1), "B"),
        ]),
        InputLifecycleError::DuplicateCategoryId(category(1))
    );
    assert_eq!(
        error(vec![
            InputCategory::new(category(1), "Cameras"),
            InputCategory::new(category(2), "cameras"),
        ])
        .code(),
        "duplicate_category_name"
    );
    assert_eq!(
        error(vec![InputCategory::new(category(1), " ")]),
        InputLifecycleError::EmptyCategoryName
    );
    assert_eq!(
        error(vec![InputCategory::new(
            category(1),
            "x".repeat(MAX_INPUT_CATEGORY_NAME_BYTES + 1)
        )])
        .code(),
        "invalid_category_name"
    );
    assert_eq!(
        error(vec![InputCategory::new(category(1), "a\tb")]),
        InputLifecycleError::CategoryNameControlCharacter
    );
    let too_many = (1..=u32::try_from(MAX_INPUT_CATEGORIES).unwrap() + 1)
        .map(|id| InputCategory::new(category(id), format!("C{id}")))
        .collect();
    assert_eq!(error(too_many).code(), "too_many_categories");
    assert!(InputCategoryId::new(0).is_none());
}

#[test]
fn lifecycle_assignments_reference_inputs_and_categories() {
    let mut project = project();
    project.set_input_categories(categories()).unwrap();
    let labelled = InputLifecycle {
        category: Some(category(2)),
        color: Some(InputColorLabel::Orange),
        paused: true,
        closed: false,
    };
    project.set_input_lifecycle(input(1), labelled).unwrap();
    assert_eq!(project.input_lifecycle(input(1)), labelled);
    assert_eq!(project.input_lifecycle(input(2)), InputLifecycle::default());
    assert_eq!(project.input_lifecycles().len(), 1);
    assert_eq!(
        project.set_input_lifecycle(input(9), labelled),
        Err(InputLifecycleError::UnknownInput(input(9)))
    );
    assert_eq!(
        project.set_input_lifecycle(
            input(2),
            InputLifecycle {
                category: Some(category(5)),
                ..InputLifecycle::default()
            }
        ),
        Err(InputLifecycleError::UnknownCategory(category(5)))
    );
    // A category still assigned cannot leave the list; an unused one can.
    assert_eq!(
        project.set_input_categories(vec![InputCategory::new(category(1), "Cameras")]),
        Err(InputLifecycleError::CategoryInUse {
            category: category(2),
            input: input(1),
        })
    );
    project
        .set_input_categories(vec![InputCategory::new(category(2), "Graphics")])
        .unwrap();
    assert!(project.validate().is_ok(), "{:?}", project.validate());
    // Resetting to the default drops the entry; removal forgets it.
    project
        .set_input_lifecycle(input(1), InputLifecycle::default())
        .unwrap();
    assert!(project.input_lifecycles().is_empty());
    project.set_input_lifecycle(input(2), labelled).unwrap();
    project.remove_input(input(2)).unwrap();
    assert!(project.input_lifecycles().is_empty());
}

#[test]
fn validation_reports_dangling_lifecycle_references() {
    let mut project = project();
    project.restore_input_lifecycle(
        input(9),
        InputLifecycle {
            paused: true,
            ..InputLifecycle::default()
        },
    );
    assert!(project.validate().is_err());
    let mut project = self::project();
    project.restore_input_lifecycle(
        input(1),
        InputLifecycle {
            category: Some(category(3)),
            ..InputLifecycle::default()
        },
    );
    assert!(project.validate().is_err());
    let mut project = self::project();
    project.restore_input_categories(vec![
        InputCategory::new(category(1), "A"),
        InputCategory::new(category(1), "B"),
    ]);
    assert!(project.validate().is_err());
}

#[test]
fn a_duplicate_copies_the_source_and_its_labels_but_starts_open() {
    let mut project = project();
    project.set_input_categories(categories()).unwrap();
    project
        .set_input_lifecycle(
            input(1),
            InputLifecycle {
                category: Some(category(1)),
                color: Some(InputColorLabel::Green),
                paused: true,
                closed: true,
            },
        )
        .unwrap();
    project
        .duplicate_input(input(1), input(7), "Camera 2".into())
        .unwrap();
    let source = project.inputs()[0].clone();
    let copy = project.inputs().last().unwrap().clone();
    assert_eq!(copy.id, input(7));
    assert_eq!(copy.name, "Camera 2");
    assert_eq!(copy.kind, source.kind);
    assert_eq!(copy.required_capabilities, source.required_capabilities);
    assert_eq!(
        project.input_lifecycle(input(7)),
        InputLifecycle {
            category: Some(category(1)),
            color: Some(InputColorLabel::Green),
            paused: false,
            closed: false,
        }
    );
    assert!(
        project
            .duplicate_input(input(9), input(8), "X".into())
            .is_err()
    );
    assert!(
        project
            .duplicate_input(input(1), input(2), "X".into())
            .is_err()
    );
    assert!(
        project
            .duplicate_input(input(1), input(8), "Camera".into())
            .is_err()
    );
}

#[test]
fn colour_labels_have_stable_tokens_and_colours() {
    assert_eq!(InputColorLabel::ALL.len(), 8);
    for label in InputColorLabel::ALL {
        assert_eq!(InputColorLabel::from_token(label.token()), Some(label));
    }
    assert_eq!(InputColorLabel::Teal.token(), "teal");
    assert_eq!(InputColorLabel::from_token("Teal"), None);
    let colours = InputColorLabel::ALL.map(InputColorLabel::rgb);
    for (index, colour) in colours.iter().enumerate() {
        assert!(!colours[..index].contains(colour));
    }
}

#[test]
fn replaceable_sources_map_to_and_from_input_kinds() {
    let playback = MediaPlaybackSettings::default();
    for source in [
        InputSourceSpec::Generator {
            video: GeneratorVideo::Solid { rgba: [1, 2, 3, 4] },
            tone_hz: Some(1_000),
        },
        InputSourceSpec::Generator {
            video: GeneratorVideo::Bars,
            tone_hz: None,
        },
        InputSourceSpec::Media {
            asset_uri: "asset://clips/a.mov".into(),
        },
        InputSourceSpec::Device {
            stable_key: "usb:cam".into(),
        },
        InputSourceSpec::Network(fm_types::network_stream::NetworkStreamConfig::new(
            "udp://@:9000",
        )),
    ] {
        assert_eq!(source.validate(), Ok(()));
        let kind = source.to_kind(playback);
        assert_eq!(InputSourceSpec::from_kind(&kind), Some(source.clone()));
        assert_eq!(source.is_media(), matches!(kind, InputKind::Media { .. }));
    }
    assert!(InputSourceSpec::from_kind(&InputKind::Color).is_none());
    assert_eq!(
        InputSourceSpec::Media {
            asset_uri: "asset://notes.txt".into(),
        }
        .validate(),
        Err(InputSourceError::UnsupportedMediaType)
    );
    assert!(
        InputSourceSpec::Media {
            asset_uri: "/host/clip.mov".into(),
        }
        .validate()
        .is_err()
    );
    assert!(
        InputSourceSpec::Device {
            stable_key: String::new(),
        }
        .validate()
        .is_err()
    );
}

#[test]
fn unique_names_take_the_next_free_suffix() {
    let taken = ["Camera", "Camera 2"];
    assert_eq!(
        unique_input_name("Camera", |name| taken.contains(&name)).as_deref(),
        Some("Camera 3")
    );
    assert_eq!(
        unique_input_name("Slides", |name| taken.contains(&name)).as_deref(),
        Some("Slides")
    );
}

#[test]
fn a_batch_import_plan_accepts_and_refuses_per_item() {
    let request = |id, name: Option<&str>, uri: &str| BatchImportRequest {
        input: input(id),
        name: name.map(str::to_owned),
        asset_uri: uri.to_owned(),
    };
    let existing = [(input(1), "Camera"), (input(2), "Opening")];
    let requests = [
        request(10, None, "asset://Opening.mov"),
        request(11, None, "/host/path.mov"),
        request(12, None, "asset://notes.txt"),
        request(13, None, "asset://Opening.mov"),
        request(1, None, "asset://other.mp4"),
        request(14, Some("Camera"), "asset://third.mp4"),
        request(15, Some(" "), "asset://fourth.mp4"),
        request(16, Some("Fifth"), "asset://fifth.mkv"),
    ];
    let plan = plan_batch_import(&existing, 1_000, &requests);
    assert_eq!(
        plan.accepted
            .iter()
            .map(|item| (item.input, item.name.as_str()))
            .collect::<Vec<_>>(),
        [(input(10), "Opening 2"), (input(16), "Fifth")]
    );
    assert_eq!(
        plan.refused,
        [
            (1, BatchImportRefusal::Unavailable),
            (2, BatchImportRefusal::UnsupportedType),
            (3, BatchImportRefusal::DuplicateSource),
            (4, BatchImportRefusal::DuplicateInput),
            (5, BatchImportRefusal::DuplicateName),
            (6, BatchImportRefusal::InvalidName),
        ]
    );
    // Capacity counts the existing inputs and earlier accepted items.
    let plan = plan_batch_import(
        &existing,
        3,
        &[
            request(10, None, "asset://a.mov"),
            request(11, None, "asset://b.mov"),
        ],
    );
    assert_eq!(plan.accepted.len(), 1);
    assert_eq!(plan.refused, [(1, BatchImportRefusal::TooManyInputs)]);
    assert_eq!(MAX_BATCH_IMPORT_ITEMS, 64);
    for refusal in BatchImportRefusal::ALL {
        assert_eq!(
            BatchImportRefusal::from_token(refusal.token()),
            Some(refusal)
        );
    }
}

/// Audio-only files (music beds and stings, IN-005) are media a batch
/// import accepts, and their names say they have no picture.
#[test]
fn audio_files_are_media_and_named_audio_only() {
    for name in [
        "asset://bed.wav",
        "asset://sting.MP3",
        "/music/bed.aac",
        "asset://beds/bed.m4a",
        "C:\\music\\bed.flac",
        "asset://bed.ogg",
        "asset://bed.opus",
    ] {
        assert!(is_media_file_name(name), "{name}");
        assert!(is_audio_file_name(name), "{name}");
    }
    for name in [
        "asset://clip.mp4",
        "asset://still.png",
        "asset://.wav",
        "asset://wav",
    ] {
        assert!(!is_audio_file_name(name), "{name}");
    }
    let plan = plan_batch_import(
        &[],
        8,
        &[BatchImportRequest {
            input: input(9),
            name: None,
            asset_uri: "asset://music-bed.wav".into(),
        }],
    );
    assert!(plan.refused.is_empty(), "{:?}", plan.refused);
    assert_eq!(plan.accepted[0].name, "music-bed");
}

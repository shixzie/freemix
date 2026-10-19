//! RC-017 cue lists (schema 48): bounded definitions (millisecond delays
//! included), consistent run states, and project-level list rules; a cue
//! that runs a project script (schema 55).

use std::num::NonZeroU128;

use fm_model::{
    Cue, CueAction, CueFault, CueList, CueListError, CueMediaOperation, CuePending, CueRunState,
    MAX_CUE_ACTIONS, MAX_CUE_DELAY_MS, MAX_CUE_LISTS, MAX_CUE_TEXT_BYTES, MAX_CUES,
    TransitionEffect, validate_cue_list, validate_cue_list_definition, validate_cue_lists,
};
use fm_types::InputId;

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn cut(value: u128) -> CueAction {
    CueAction::Take {
        input: Some(input(value)),
        effect: TransitionEffect::Cut,
        duration_frames: 0,
    }
}

fn cue(id: u64, actions: Vec<CueAction>) -> Cue {
    Cue {
        id,
        name: format!("Cue {id}"),
        actions,
        auto_follow: false,
    }
}

fn list(id: u64, cues: Vec<Cue>, state: CueRunState) -> CueList {
    CueList {
        id,
        name: "Show".into(),
        cues,
        state,
    }
}

#[test]
fn definitions_enforce_names_bounds_and_one_take_per_frame() {
    let valid = vec![
        cue(1, vec![cut(1), CueAction::Wait { frames: 25 }, cut(2)]),
        cue(2, Vec::new()),
    ];
    assert_eq!(validate_cue_list_definition("Show", &valid), Ok(()));
    assert_eq!(
        validate_cue_list_definition("  ", &valid),
        Err(CueListError::InvalidName { cue: None })
    );
    let mut blank = valid.clone();
    blank[1].name = String::new();
    assert_eq!(
        validate_cue_list_definition("Show", &blank),
        Err(CueListError::InvalidName { cue: Some(2) })
    );
    assert_eq!(
        validate_cue_list_definition("Show", &[cue(3, Vec::new()), cue(3, Vec::new())]),
        Err(CueListError::DuplicateCue(3))
    );
    let many: Vec<_> = (0..=MAX_CUES as u64)
        .map(|id| cue(id, Vec::new()))
        .collect();
    assert_eq!(
        validate_cue_list_definition("Show", &many),
        Err(CueListError::TooManyCues {
            count: MAX_CUES + 1
        })
    );
    assert_eq!(
        validate_cue_list_definition(
            "Show",
            &[cue(
                4,
                vec![CueAction::Wait { frames: 1 }; MAX_CUE_ACTIONS + 1]
            )]
        ),
        Err(CueListError::TooManyActions { cue: 4 })
    );
    // Two takes between waits would fight over one frame's Program.
    assert_eq!(
        validate_cue_list_definition("Show", &[cue(5, vec![cut(1), cut(2)])]),
        Err(CueListError::ConflictingTakes { cue: 5 })
    );
    for (action, index) in [
        (CueAction::Wait { frames: 0 }, 0),
        (CueAction::Delay { millis: 0 }, 0),
        (
            CueAction::Delay {
                millis: MAX_CUE_DELAY_MS + 1,
            },
            0,
        ),
        (
            CueAction::Take {
                input: None,
                effect: TransitionEffect::Cut,
                duration_frames: 5,
            },
            0,
        ),
        (CueAction::OverlayOff { channel: 9 }, 0),
        (
            CueAction::FadeToBlack {
                active: true,
                duration_frames: 0,
            },
            0,
        ),
        (
            CueAction::TitleText {
                input: input(1),
                field: fm_model::TitleFieldId::new(NonZeroU128::new(1).unwrap()),
                text: "x".repeat(MAX_CUE_TEXT_BYTES + 1),
            },
            0,
        ),
    ] {
        assert_eq!(
            validate_cue_list_definition("Show", &[cue(6, vec![action.clone()])]),
            Err(CueListError::InvalidAction {
                cue: 6,
                action: index
            }),
            "{action:?}"
        );
    }
}

#[test]
fn delays_separate_frames_and_media_actions_validate() {
    // A millisecond delay separates frames like a frame wait does.
    assert_eq!(
        validate_cue_list_definition(
            "Show",
            &[cue(
                7,
                vec![
                    cut(1),
                    CueAction::Delay { millis: 500 },
                    cut(2),
                    CueAction::Media {
                        input: input(3),
                        operation: CueMediaOperation::Seek { frame: 9 },
                    },
                ]
            )]
        ),
        Ok(())
    );
}

#[test]
fn run_states_name_only_held_cues_and_resume_after_a_wait() {
    let cues = || {
        vec![
            cue(1, vec![cut(1), CueAction::Wait { frames: 25 }, cut(2)]),
            cue(2, vec![cut(1)]),
        ]
    };
    let waiting = CueRunState {
        standby: Some(2),
        live: Some(1),
        pending: Some(CuePending {
            cue: 1,
            action: 2,
            due_frame: 50,
        }),
        played: vec![1],
        fault: None,
    };
    assert_eq!(validate_cue_list(&list(1, cues(), waiting.clone())), Ok(()));
    // A fault keeps the refusal that stopped its cue (schema 57).
    let refused = |message: &str| fm_command::FaultDetail {
        reason: Some(fm_command::RefusalReason::ProjectLocked),
        message: message.to_owned(),
    };
    let faulted = CueRunState {
        pending: None,
        fault: Some(CueFault {
            cue: 1,
            action: 2,
            detail: refused("the project padlock is engaged"),
        }),
        ..waiting.clone()
    };
    assert_eq!(validate_cue_list(&list(1, cues(), faulted)), Ok(()));
    for state in [
        CueRunState {
            standby: Some(9),
            ..CueRunState::default()
        },
        CueRunState {
            pending: Some(CuePending {
                cue: 1,
                action: 1,
                due_frame: 50,
            }),
            ..waiting.clone()
        },
        CueRunState {
            played: vec![2, 1],
            ..waiting.clone()
        },
        CueRunState {
            fault: Some(CueFault {
                cue: 2,
                action: 1,
                detail: refused("refused"),
            }),
            ..waiting.clone()
        },
        CueRunState {
            fault: Some(CueFault {
                cue: 1,
                action: 2,
                detail: refused(""),
            }),
            ..waiting.clone()
        },
        CueRunState {
            fault: Some(CueFault {
                cue: 1,
                action: 2,
                detail: refused(&"x".repeat(fm_command::MAX_FAULT_MESSAGE_BYTES + 1)),
            }),
            ..waiting.clone()
        },
        CueRunState {
            fault: Some(CueFault {
                cue: 1,
                action: 2,
                detail: refused("two\nlines"),
            }),
            ..waiting
        },
    ] {
        assert_eq!(
            validate_cue_list(&list(1, cues(), state.clone())),
            Err(CueListError::InvalidState),
            "{state:?}"
        );
    }
}

#[test]
fn projects_hold_a_bounded_set_of_uniquely_numbered_lists() {
    let lists: Vec<_> = (1..=MAX_CUE_LISTS as u64)
        .map(|id| list(id, Vec::new(), CueRunState::default()))
        .collect();
    assert_eq!(validate_cue_lists(&lists), Ok(()));
    let mut too_many = lists.clone();
    too_many.push(list(99, Vec::new(), CueRunState::default()));
    assert_eq!(validate_cue_lists(&too_many), Err(None));
    let duplicate = vec![
        list(1, Vec::new(), CueRunState::default()),
        list(1, Vec::new(), CueRunState::default()),
    ];
    assert_eq!(validate_cue_lists(&duplicate), Err(None));
    // List 0 is the cue clock's waiting script run (schema 56).
    let zero = vec![list(0, Vec::new(), CueRunState::default())];
    assert_eq!(validate_cue_lists(&zero), Err(None));
    let mut project = project();
    project.set_cue_lists(zero);
    assert_eq!(
        project.validate().unwrap_err()[0].kind,
        fm_model::ValidationErrorKind::OutOfRange
    );
    let mut invalid = list(2, Vec::new(), CueRunState::default());
    invalid.name = String::new();
    assert_eq!(
        validate_cue_lists(&[invalid]),
        Err(Some(CueListError::InvalidName { cue: None }))
    );
}

fn following(id: u64, actions: Vec<CueAction>) -> Cue {
    Cue {
        auto_follow: true,
        ..cue(id, actions)
    }
}

/// The actions up to the first wait run on one frame, and an auto-follow
/// with no wait before it carries that frame into the next cue: two takes
/// on that frame are refused naming both cues.
#[test]
fn auto_follow_chains_take_program_at_most_once_per_frame() {
    assert_eq!(
        validate_cue_list_definition("Show", &[following(1, vec![cut(1)]), cue(2, vec![cut(2)])]),
        Err(CueListError::ConflictingFollowTakes {
            cue: 1,
            follower: 2
        })
    );
    // Through a cue without a take, and into a playlist start.
    assert_eq!(
        validate_cue_list_definition(
            "Show",
            &[
                following(1, vec![cut(1)]),
                following(2, vec![CueAction::OverlayOff { channel: 1 }]),
                cue(
                    3,
                    vec![CueAction::PlaylistStart, CueAction::Wait { frames: 1 }]
                ),
            ]
        ),
        Err(CueListError::ConflictingFollowTakes {
            cue: 1,
            follower: 3
        })
    );
    // The follower's take after its own first wait, the leader's take
    // before its last wait, a trailing wait, or no auto-follow: all valid.
    for cues in [
        vec![
            following(1, vec![cut(1)]),
            cue(2, vec![CueAction::Wait { frames: 1 }, cut(2)]),
        ],
        vec![
            following(
                1,
                vec![
                    cut(1),
                    CueAction::Wait { frames: 1 },
                    CueAction::PlaylistStop,
                ],
            ),
            cue(2, vec![cut(2)]),
        ],
        vec![
            following(1, vec![cut(1), CueAction::Delay { millis: 40 }]),
            cue(2, vec![cut(2)]),
        ],
        vec![cue(1, vec![cut(1)]), cue(2, vec![cut(2)])],
    ] {
        assert_eq!(
            validate_cue_list_definition("Show", &cues),
            Ok(()),
            "{cues:?}"
        );
    }
    assert_eq!(
        CueListError::ConflictingFollowTakes {
            cue: 1,
            follower: 2
        }
        .to_string(),
        "cue 1 auto-follows into cue 2 and both take Program on one frame"
    );
}

fn project() -> fm_model::Project {
    use fm_types::{
        AudioFormat, ChannelLayout, ColorMetadata, FrameRate, PixelFormat, ProjectId, SampleFormat,
        SampleRate, ScanMode, VideoDimensions, VideoFormat,
    };
    let frame_rate = FrameRate::new(30, 1).unwrap();
    fm_model::Project::new(
        ProjectId::new(NonZeroU128::MIN),
        "Show",
        fm_model::ProjectSettings {
            frame_rate,
            video: VideoFormat {
                dimensions: VideoDimensions::new(1920, 1080).unwrap(),
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

/// A cue may run a project script (schema 55): the id is nonzero and names
/// a script the project holds. The step is not counted as a take, since
/// what the script takes is known only when it runs.
#[test]
fn a_cue_runs_a_script_the_project_holds() {
    let runs = CueAction::RunScript { script: 7 };
    assert_eq!(runs.script(), Some(7));
    assert_eq!(runs.input(), None);
    assert!(!runs.takes_program() && !runs.is_wait());
    assert!(fm_model::cue_action_is_valid(&runs));
    assert!(!fm_model::cue_action_is_valid(&CueAction::RunScript {
        script: 0
    }));
    // A list runs at most MAX_CUE_LIST_SCRIPT_STEPS scripts.
    let steps = |count: usize| vec![cue(1, vec![CueAction::RunScript { script: 7 }; count])];
    assert_eq!(
        validate_cue_list_definition("Show", &steps(fm_model::MAX_CUE_LIST_SCRIPT_STEPS)),
        Ok(())
    );
    assert_eq!(
        validate_cue_list_definition("Show", &steps(fm_model::MAX_CUE_LIST_SCRIPT_STEPS + 1)),
        Err(CueListError::TooManyScriptSteps {
            count: fm_model::MAX_CUE_LIST_SCRIPT_STEPS + 1
        })
    );
    // A take beside a script step is a valid definition.
    assert_eq!(
        validate_cue_list_definition("Show", &[cue(1, vec![runs.clone(), cut(1)])]),
        Ok(())
    );

    let mut project = project();
    project.set_cue_lists(vec![list(
        1,
        vec![cue(1, vec![runs])],
        CueRunState {
            standby: Some(1),
            ..CueRunState::default()
        },
    )]);
    let missing = fm_model::EntityRef::Script(7);
    let errors = project.validate().unwrap_err();
    assert!(
        errors.iter().any(|error| error.entity == Some(missing)
            && error.field == "cue_lists.cues.actions.script"
            && error.kind == fm_model::ValidationErrorKind::MissingReference(missing)),
        "{errors:?}"
    );
    project.set_scripts(vec![fm_model::Script {
        id: 7,
        name: "Open".into(),
        source: "cut 1\n".into(),
    }]);
    assert_eq!(project.validate(), Ok(()));
}

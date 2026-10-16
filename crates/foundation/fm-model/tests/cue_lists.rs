//! RC-017 cue lists (schema 48): bounded definitions (millisecond delays
//! included), consistent run states, and project-level list rules.

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
            fault: Some(CueFault { cue: 2, action: 1 }),
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
    let mut invalid = list(2, Vec::new(), CueRunState::default());
    invalid.name = String::new();
    assert_eq!(
        validate_cue_lists(&[invalid]),
        Err(Some(CueListError::InvalidName { cue: None }))
    );
}
